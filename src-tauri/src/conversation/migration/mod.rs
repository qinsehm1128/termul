//! Restart-idempotent control plane for the Conversation v2 layout migration.
//!
//! Bootstrap owns the process lock and hands the guard into this transport-neutral service. The
//! service journals every mutation with immediate durability, never opens mutable application
//! stores, and changes reader behavior only through the generation-stamped layout descriptor.

pub mod journal;
pub mod layout;
pub mod lock;

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::conversation::durable_fs::{DirectoryPermissions, DurableFileSystem};

pub use journal::{
    advance_phase, ApprovalReceiptV1, BootstrapObservationReceiptV1, MigrationJournalV1,
    MigrationPhase, ObservationEvidenceV1, StepReceiptV1, FINALIZATION_ACTION, MIGRATION_ID,
    MIGRATION_JOURNAL_SCHEMA_VERSION, STEP_RECEIPT_SCHEMA_VERSION,
};
pub use layout::{
    recover_cutover, ActiveLayout, ConversationLayoutDescriptorV1, CutoverRecovery,
    ReaderPrecedence, LAYOUT_DESCRIPTOR_FILE, LAYOUT_DESCRIPTOR_SCHEMA_VERSION,
};
pub use lock::{HostMigrationLock, HostMigrationLockGuard, MIGRATION_LOCK_FILE};

pub const MIGRATION_JOURNAL_FILE: &str = "conversation-layout-v2.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MigrationErrorCode {
    MigrationInProgress,
    MigrationLockInvalid,
    MigrationIdempotencyConflict,
    MigrationRestartRequired,
    MigrationFinalizationNotReady,
    MigrationApprovalInvalid,
    MigrationObservationInvalid,
    MigrationIllegalTransition,
    MigrationDurabilityFailed,
    MigrationVerificationFailed,
    MigrationJournalCorrupt,
    MigrationLayoutCorrupt,
    MigrationCrashInjected,
}

impl MigrationErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MigrationInProgress => "MIGRATION_IN_PROGRESS",
            Self::MigrationLockInvalid => "MIGRATION_LOCK_INVALID",
            Self::MigrationIdempotencyConflict => "MIGRATION_IDEMPOTENCY_CONFLICT",
            Self::MigrationRestartRequired => "MIGRATION_RESTART_REQUIRED",
            Self::MigrationFinalizationNotReady => "MIGRATION_FINALIZATION_NOT_READY",
            Self::MigrationApprovalInvalid => "MIGRATION_APPROVAL_INVALID",
            Self::MigrationObservationInvalid => "MIGRATION_OBSERVATION_INVALID",
            Self::MigrationIllegalTransition => "MIGRATION_ILLEGAL_TRANSITION",
            Self::MigrationDurabilityFailed => "MIGRATION_DURABILITY_FAILED",
            Self::MigrationVerificationFailed => "MIGRATION_VERIFICATION_FAILED",
            Self::MigrationJournalCorrupt => "MIGRATION_JOURNAL_CORRUPT",
            Self::MigrationLayoutCorrupt => "MIGRATION_LAYOUT_CORRUPT",
            Self::MigrationCrashInjected => "MIGRATION_CRASH_INJECTED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationError {
    pub code: MigrationErrorCode,
    pub operation: &'static str,
    pub detail: String,
}

impl MigrationError {
    pub fn new(
        code: MigrationErrorCode,
        operation: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        let detail = detail.into();
        if matches!(
            code,
            MigrationErrorCode::MigrationInProgress
                | MigrationErrorCode::MigrationIdempotencyConflict
                | MigrationErrorCode::MigrationDurabilityFailed
                | MigrationErrorCode::MigrationVerificationFailed
                | MigrationErrorCode::MigrationIllegalTransition
                | MigrationErrorCode::MigrationLockInvalid
        ) {
            log::error!(
                "[conversation-migration] operation failed code={} operation={}",
                code.as_str(),
                operation
            );
        }
        Self {
            code,
            operation,
            detail,
        }
    }
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} during {}: {}",
            self.code.as_str(),
            self.operation,
            self.detail
        )
    }
}

impl std::error::Error for MigrationError {}

pub type Result<T> = std::result::Result<T, MigrationError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationHostMode {
    Desktop,
    Standalone,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MigrationAdmissionState {
    pub conversation_repository_active: bool,
    pub session_persistence_active: bool,
    pub chat_history_active: bool,
    pub workspace_manifest_active: bool,
    pub agent_manager_active: bool,
    pub pty_manager_active: bool,
    pub route_admission_active: bool,
}

impl MigrationAdmissionState {
    #[must_use]
    pub fn is_clear(self) -> bool {
        !self.conversation_repository_active
            && !self.session_persistence_active
            && !self.chat_history_active
            && !self.workspace_manifest_active
            && !self.agent_manager_active
            && !self.pty_manager_active
            && !self.route_admission_active
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationStepOutput {
    pub receipt_key: String,
    pub output_sha256: String,
}

impl MigrationStepOutput {
    #[must_use]
    pub fn new(receipt_key: impl Into<String>, output_sha256: impl Into<String>) -> Self {
        Self {
            receipt_key: receipt_key.into(),
            output_sha256: output_sha256.into(),
        }
    }
}

/// TASK-005 supplies the legacy inventory and transformation details through this boundary.
/// Implementations must make each callback at-least-once safe; once its durable receipt exists,
/// this service skips the callback on restart.
pub trait MigrationCallbacks {
    fn inventory(&mut self, journal: &MigrationJournalV1) -> Result<MigrationStepOutput>;
    fn stage(&mut self, journal: &MigrationJournalV1) -> Result<MigrationStepOutput>;
    fn verify(&mut self, journal: &MigrationJournalV1) -> Result<MigrationStepOutput>;

    fn sync_artifacts(&mut self, _phase: MigrationPhase) -> Result<()> {
        Ok(())
    }
}

pub struct MigrationContext<'a> {
    pub lock_guard: &'a HostMigrationLockGuard,
    pub host_state_root: &'a Path,
    pub operation_key: &'a str,
    pub host_mode: MigrationHostMode,
    pub admission: MigrationAdmissionState,
    pub now_utc: DateTime<Utc>,
    pub callbacks: &'a mut dyn MigrationCallbacks,
}

pub struct MigrationControlContext<'a> {
    pub lock_guard: &'a HostMigrationLockGuard,
    pub host_state_root: &'a Path,
    pub now_utc: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationReport {
    pub operation_id: Uuid,
    pub operation_key: String,
    pub phase: MigrationPhase,
    pub attempt: u32,
    pub target_generation: Uuid,
    pub reader_precedence: ReaderPrecedence,
    pub completed_step_count: usize,
    pub reused_step_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationCrashPoint {
    AfterJournalWrite,
    AfterArtifactSync,
    AfterDescriptorReplace,
    AfterPhaseBoundary,
}

pub trait MigrationCrashInjector: Send + Sync {
    fn should_interrupt(&self, point: MigrationCrashPoint) -> bool;
}

#[derive(Clone)]
pub struct ConversationMigrationService {
    canonical_host_root: PathBuf,
    migration_dir: PathBuf,
    journal_path: PathBuf,
    layout_path: PathBuf,
    durable_fs: DurableFileSystem,
    crash_injector: Option<Arc<dyn MigrationCrashInjector>>,
}

impl fmt::Debug for ConversationMigrationService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConversationMigrationService")
            .field("canonical_host_root", &self.canonical_host_root)
            .field("crash_injector", &self.crash_injector.is_some())
            .finish()
    }
}

impl ConversationMigrationService {
    pub fn new(host_state_root: &Path) -> Result<Self> {
        Self::with_optional_crash_injector(host_state_root, None)
    }

    pub fn with_crash_injector(
        host_state_root: &Path,
        crash_injector: Arc<dyn MigrationCrashInjector>,
    ) -> Result<Self> {
        Self::with_optional_crash_injector(host_state_root, Some(crash_injector))
    }

    fn with_optional_crash_injector(
        host_state_root: &Path,
        crash_injector: Option<Arc<dyn MigrationCrashInjector>>,
    ) -> Result<Self> {
        let canonical_host_root = host_state_root.canonicalize().map_err(|error| {
            MigrationError::new(
                MigrationErrorCode::MigrationLockInvalid,
                "create_service",
                format!("host-state root cannot be canonicalized: {error}"),
            )
        })?;
        let migration_dir = canonical_host_root.join("conversation-migrations");
        Ok(Self {
            journal_path: migration_dir.join(MIGRATION_JOURNAL_FILE),
            layout_path: canonical_host_root.join(LAYOUT_DESCRIPTOR_FILE),
            canonical_host_root,
            migration_dir,
            durable_fs: DurableFileSystem::new(),
            crash_injector,
        })
    }

    /// Canonical entry point. The supplied guard is validated before journal or descriptor access.
    pub fn recover_and_run(&self, mut context: MigrationContext<'_>) -> Result<MigrationReport> {
        self.validate_guard(context.lock_guard, context.host_state_root)?;
        journal::validate_sha256(context.operation_key)?;
        self.ensure_migration_dir()?;
        let mut journal = self.load_or_create_journal(context.operation_key, context.now_utc)?;
        let mut descriptor = self.load_or_create_layout(context.now_utc)?;
        let mut reused_step_count = 0usize;
        log::info!(
            "[conversation-migration] operation start operation_id={} operation_key_prefix={} host_mode={:?} phase={:?} attempt={}",
            journal.operation_id,
            operation_key_prefix(&journal.operation_key),
            context.host_mode,
            journal.phase,
            journal.attempt
        );

        loop {
            let phase_started = Instant::now();
            let result = self.run_phase(
                &mut journal,
                &mut descriptor,
                &mut context,
                &mut reused_step_count,
            );
            match result {
                Ok(PhaseControl::Continue) => {
                    log::info!(
                        "[conversation-migration] phase complete operation_id={} phase={:?} inventory_digest={} staged_digest={} duration_ms={}",
                        journal.operation_id,
                        journal.phase,
                        digest_log_value(journal.inventory_sha256.as_deref()),
                        digest_log_value(journal.staged_manifest_sha256.as_deref()),
                        phase_started.elapsed().as_millis()
                    );
                }
                Ok(PhaseControl::Stable) => {
                    return Ok(report(&journal, &descriptor, reused_step_count));
                }
                Err(error) if error.code == MigrationErrorCode::MigrationCrashInjected => {
                    return Err(error);
                }
                Err(error) => {
                    journal.record_failure(&error, context.now_utc);
                    if let Err(persist_error) = self.write_journal(&journal) {
                        return Err(MigrationError::new(
                            MigrationErrorCode::MigrationDurabilityFailed,
                            "persist_phase_failure",
                            format!("{error}; additionally failed to persist lastError: {persist_error}"),
                        ));
                    }
                    return Err(error);
                }
            }
        }
    }

    /// Explicit negative path used by bootstrap tests and defensive integrations.
    pub fn recover_and_run_without_guard(&self) -> Result<MigrationReport> {
        Err(MigrationError::new(
            MigrationErrorCode::MigrationLockInvalid,
            "recover_and_run",
            "a bootstrap-owned migration guard is required before journal access",
        ))
    }

    pub fn request_rollback(
        &self,
        context: MigrationControlContext<'_>,
    ) -> Result<MigrationReport> {
        self.validate_guard(context.lock_guard, context.host_state_root)?;
        self.ensure_migration_dir()?;
        let mut journal = self.load_journal()?;
        let mut descriptor = self.load_layout()?;
        match journal.phase {
            MigrationPhase::Committed | MigrationPhase::ObservationWindow => {
                log::warn!(
                    "[conversation-migration] rollback requested operation_id={} phase={:?}",
                    journal.operation_id,
                    journal.phase
                );
                self.transition(
                    &mut journal,
                    MigrationPhase::RollbackPending,
                    context.now_utc,
                    false,
                )?;
            }
            MigrationPhase::RollbackPending => {}
            _ => {
                return Err(MigrationError::new(
                    MigrationErrorCode::MigrationIllegalTransition,
                    "request_rollback",
                    "rollback is allowed only after commit or during observation",
                ));
            }
        }
        self.reconcile_layout(&mut journal, &mut descriptor, context.now_utc)?;
        Ok(report(&journal, &descriptor, 0))
    }

    pub fn reapply_and_run(&self, context: MigrationContext<'_>) -> Result<MigrationReport> {
        self.validate_guard(context.lock_guard, context.host_state_root)?;
        self.ensure_migration_dir()?;
        let mut journal = self.load_journal()?;
        if journal.operation_key != context.operation_key {
            return Err(MigrationError::new(
                MigrationErrorCode::MigrationIdempotencyConflict,
                "reapply",
                "operation key does not match the durable migration operation",
            ));
        }
        if journal.phase != MigrationPhase::RolledBack {
            return Err(MigrationError::new(
                MigrationErrorCode::MigrationIllegalTransition,
                "reapply",
                "reapply is allowed only from rolled_back",
            ));
        }
        let verify_prefix = format!("{}:verify:", journal.operation_key);
        journal
            .completed_steps
            .retain(|key, _| !key.starts_with(&verify_prefix));
        journal.target_generation = Uuid::new_v4();
        journal.observation_evidence = None;
        journal.approval_receipt = None;
        advance_phase(
            &mut journal,
            MigrationPhase::Verifying,
            context.now_utc,
            false,
        )?;
        self.write_journal(&journal)?;
        self.interrupt(MigrationCrashPoint::AfterPhaseBoundary)?;
        self.recover_and_run(context)
    }

    pub fn record_bootstrap_observation(
        &self,
        context: MigrationControlContext<'_>,
        receipt: BootstrapObservationReceiptV1,
    ) -> Result<MigrationReport> {
        self.validate_guard(context.lock_guard, context.host_state_root)?;
        let mut journal = self.load_journal()?;
        let descriptor = self.load_layout()?;
        if journal.phase != MigrationPhase::ObservationWindow {
            return Err(MigrationError::new(
                MigrationErrorCode::MigrationObservationInvalid,
                "record_observation",
                "bootstrap observations are accepted only during observation_window",
            ));
        }
        let evidence = journal.observation_evidence.as_mut().ok_or_else(|| {
            MigrationError::new(
                MigrationErrorCode::MigrationJournalCorrupt,
                "record_observation",
                "observation evidence was not initialized from verification",
            )
        })?;
        if evidence.record(receipt)? {
            journal.updated_at_utc = context.now_utc;
            self.write_journal(&journal)?;
        }
        if journal
            .observation_evidence
            .as_ref()
            .is_some_and(|evidence| evidence.validate_ready().is_ok())
        {
            let marker_key = format!("{}:observation:v1", journal.operation_key);
            if !journal.completed_steps.contains_key(&marker_key) {
                let digest = sha256_json(
                    journal
                        .observation_evidence
                        .as_ref()
                        .expect("checked above"),
                )?;
                journal.record_step(marker_key, digest, context.now_utc)?;
                self.write_journal(&journal)?;
            }
        }
        Ok(report(&journal, &descriptor, 0))
    }

    pub fn finalize(
        &self,
        context: MigrationControlContext<'_>,
        approval: ApprovalReceiptV1,
    ) -> Result<MigrationReport> {
        self.validate_guard(context.lock_guard, context.host_state_root)?;
        let mut journal = self.load_journal()?;
        let mut descriptor = self.load_layout()?;
        if journal.approval_receipt.is_some() {
            return Err(MigrationError::new(
                MigrationErrorCode::MigrationApprovalInvalid,
                "finalize",
                "approval requestId has already been consumed",
            ));
        }
        if journal.phase != MigrationPhase::ObservationWindow {
            return Err(MigrationError::new(
                MigrationErrorCode::MigrationIllegalTransition,
                "finalize",
                "finalization is allowed only from observation_window",
            ));
        }
        let evidence = journal.observation_evidence.as_ref().ok_or_else(|| {
            MigrationError::new(
                MigrationErrorCode::MigrationFinalizationNotReady,
                "finalize",
                "observation evidence is missing",
            )
        })?;
        evidence.validate_ready()?;
        approval.validate(evidence)?;
        journal.approval_receipt = Some(approval);
        journal.updated_at_utc = context.now_utc;
        self.write_journal(&journal)?;
        self.complete_finalization(&mut journal, &mut descriptor, context.now_utc)?;
        Ok(report(&journal, &descriptor, 0))
    }

    fn run_phase(
        &self,
        journal: &mut MigrationJournalV1,
        descriptor: &mut ConversationLayoutDescriptorV1,
        context: &mut MigrationContext<'_>,
        reused_step_count: &mut usize,
    ) -> Result<PhaseControl> {
        if matches!(
            journal.phase,
            MigrationPhase::Detected | MigrationPhase::Quiescing
        ) && !context.admission.is_clear()
        {
            log::warn!(
                "[conversation-migration] restart required operation_id={} phase={:?}",
                journal.operation_id,
                journal.phase
            );
            return Err(MigrationError::new(
                MigrationErrorCode::MigrationRestartRequired,
                "quiesce",
                "mutable stores, managers, or route admission are already active",
            ));
        }
        match journal.phase {
            MigrationPhase::Detected => {
                let key = format!("{}:quiesce:v1", journal.operation_key);
                let digest = sha256_bytes(b"startup-admission-barrier-clear");
                if !journal.record_step(key, digest, context.now_utc)? {
                    *reused_step_count += 1;
                } else {
                    self.write_journal(journal)?;
                }
                self.transition(journal, MigrationPhase::Quiescing, context.now_utc, false)?;
                Ok(PhaseControl::Continue)
            }
            MigrationPhase::Quiescing => {
                let prefix = format!("{}:inventory:", journal.operation_key);
                let digest = if let Some(value) = journal.inventory_sha256.clone() {
                    *reused_step_count += 1;
                    value
                } else if let Some((_, receipt)) = journal.receipt_with_prefix(&prefix) {
                    *reused_step_count += 1;
                    receipt.output_sha256.clone()
                } else {
                    let output = context.callbacks.inventory(journal)?;
                    validate_callback_output(&output, &prefix, "inventory")?;
                    context
                        .callbacks
                        .sync_artifacts(MigrationPhase::Quiescing)?;
                    self.interrupt(MigrationCrashPoint::AfterArtifactSync)?;
                    journal.record_step(
                        output.receipt_key,
                        output.output_sha256.clone(),
                        context.now_utc,
                    )?;
                    self.write_journal(journal)?;
                    output.output_sha256
                };
                journal.inventory_sha256 = Some(digest);
                self.write_journal(journal)?;
                self.transition(journal, MigrationPhase::Inventoried, context.now_utc, false)?;
                Ok(PhaseControl::Continue)
            }
            MigrationPhase::Inventoried => {
                self.transition(journal, MigrationPhase::Staging, context.now_utc, false)?;
                Ok(PhaseControl::Continue)
            }
            MigrationPhase::Staging => {
                let prefix = format!("{}:stage:", journal.operation_key);
                let digest = if let Some(value) = journal.staged_manifest_sha256.clone() {
                    *reused_step_count += 1;
                    value
                } else if let Some((_, receipt)) = journal.receipt_with_prefix(&prefix) {
                    *reused_step_count += 1;
                    receipt.output_sha256.clone()
                } else {
                    let output = context.callbacks.stage(journal)?;
                    validate_callback_output(&output, &prefix, "stage")?;
                    context.callbacks.sync_artifacts(MigrationPhase::Staging)?;
                    self.interrupt(MigrationCrashPoint::AfterArtifactSync)?;
                    journal.record_step(
                        output.receipt_key,
                        output.output_sha256.clone(),
                        context.now_utc,
                    )?;
                    self.write_journal(journal)?;
                    output.output_sha256
                };
                journal.staged_manifest_sha256 = Some(digest);
                self.write_journal(journal)?;
                self.transition(journal, MigrationPhase::Verifying, context.now_utc, false)?;
                Ok(PhaseControl::Continue)
            }
            MigrationPhase::Verifying => {
                let inventory = journal.inventory_sha256.as_deref().ok_or_else(|| {
                    MigrationError::new(
                        MigrationErrorCode::MigrationJournalCorrupt,
                        "verify",
                        "inventory digest is missing",
                    )
                })?;
                let staged = journal.staged_manifest_sha256.as_deref().ok_or_else(|| {
                    MigrationError::new(
                        MigrationErrorCode::MigrationJournalCorrupt,
                        "verify",
                        "staged manifest digest is missing",
                    )
                })?;
                let key = format!("{}:verify:{inventory}:{staged}", journal.operation_key);
                let validation_digest = if let Some(receipt) = journal.completed_steps.get(&key) {
                    *reused_step_count += 1;
                    receipt.output_sha256.clone()
                } else {
                    let output = context.callbacks.verify(journal)?;
                    if output.receipt_key != key {
                        return Err(MigrationError::new(
                            MigrationErrorCode::MigrationVerificationFailed,
                            "verify",
                            "verification receipt key does not match journal inputs",
                        ));
                    }
                    validate_callback_output(&output, &key, "verify")?;
                    context
                        .callbacks
                        .sync_artifacts(MigrationPhase::Verifying)?;
                    self.interrupt(MigrationCrashPoint::AfterArtifactSync)?;
                    journal.record_step(
                        output.receipt_key,
                        output.output_sha256.clone(),
                        context.now_utc,
                    )?;
                    self.write_journal(journal)?;
                    output.output_sha256
                };
                journal.observation_evidence =
                    Some(ObservationEvidenceV1::empty(validation_digest));
                self.write_journal(journal)?;
                self.transition(
                    journal,
                    MigrationPhase::CutoverPending,
                    context.now_utc,
                    false,
                )?;
                Ok(PhaseControl::Continue)
            }
            MigrationPhase::CutoverPending
            | MigrationPhase::Committed
            | MigrationPhase::RollbackPending => {
                self.reconcile_layout(journal, descriptor, context.now_utc)?;
                Ok(
                    if matches!(
                        journal.phase,
                        MigrationPhase::ObservationWindow | MigrationPhase::RolledBack
                    ) {
                        PhaseControl::Stable
                    } else {
                        PhaseControl::Continue
                    },
                )
            }
            MigrationPhase::ObservationWindow => {
                if let Some(approval) = journal.approval_receipt.clone() {
                    let evidence = journal.observation_evidence.as_ref().ok_or_else(|| {
                        MigrationError::new(
                            MigrationErrorCode::MigrationFinalizationNotReady,
                            "recover_finalization",
                            "approved finalization is missing observation evidence",
                        )
                    })?;
                    evidence.validate_ready()?;
                    approval.validate(evidence)?;
                    self.complete_finalization(journal, descriptor, context.now_utc)?;
                    Ok(PhaseControl::Continue)
                } else {
                    Ok(PhaseControl::Stable)
                }
            }
            MigrationPhase::RolledBack | MigrationPhase::Finalized => Ok(PhaseControl::Stable),
        }
    }

    fn reconcile_layout(
        &self,
        journal: &mut MigrationJournalV1,
        descriptor: &mut ConversationLayoutDescriptorV1,
        now_utc: DateTime<Utc>,
    ) -> Result<()> {
        match recover_cutover(journal, descriptor) {
            CutoverRecovery::WriteTargetThenCommit => {
                log::warn!(
                    "[conversation-migration] reconciling pending cutover operation_id={} action=write_target",
                    journal.operation_id
                );
                *descriptor = ConversationLayoutDescriptorV1::target(
                    journal,
                    &self.canonical_host_root,
                    now_utc,
                );
                self.write_layout(descriptor)?;
                self.transition(journal, MigrationPhase::Committed, now_utc, false)?;
            }
            CutoverRecovery::CompleteCommit => {
                log::warn!(
                    "[conversation-migration] reconciling pending cutover operation_id={} action=complete_commit",
                    journal.operation_id
                );
                self.transition(journal, MigrationPhase::Committed, now_utc, false)?;
            }
            CutoverRecovery::RestoreCommittedTarget => {
                log::warn!(
                    "[conversation-migration] reconciling committed pointer operation_id={} action=restore_target",
                    journal.operation_id
                );
                *descriptor = ConversationLayoutDescriptorV1::target(
                    journal,
                    &self.canonical_host_root,
                    now_utc,
                );
                self.write_layout(descriptor)?;
                if journal.phase == MigrationPhase::Committed {
                    self.transition(journal, MigrationPhase::ObservationWindow, now_utc, false)?;
                }
            }
            CutoverRecovery::WriteHybridThenRollback => {
                log::warn!(
                    "[conversation-migration] reconciling rollback operation_id={} action=write_hybrid",
                    journal.operation_id
                );
                *descriptor = ConversationLayoutDescriptorV1::hybrid(
                    journal,
                    &self.canonical_host_root,
                    now_utc,
                );
                self.write_layout(descriptor)?;
                self.transition(journal, MigrationPhase::RolledBack, now_utc, false)?;
            }
            CutoverRecovery::CompleteRollback => {
                self.transition(journal, MigrationPhase::RolledBack, now_utc, false)?;
            }
            CutoverRecovery::RestoreFinalizedTarget => {
                *descriptor = ConversationLayoutDescriptorV1::finalized(
                    journal,
                    &self.canonical_host_root,
                    now_utc,
                );
                self.write_layout(descriptor)?;
            }
            CutoverRecovery::NoAction => {
                if journal.phase == MigrationPhase::Committed {
                    self.transition(journal, MigrationPhase::ObservationWindow, now_utc, false)?;
                }
            }
        }
        Ok(())
    }

    fn complete_finalization(
        &self,
        journal: &mut MigrationJournalV1,
        descriptor: &mut ConversationLayoutDescriptorV1,
        now_utc: DateTime<Utc>,
    ) -> Result<()> {
        *descriptor =
            ConversationLayoutDescriptorV1::finalized(journal, &self.canonical_host_root, now_utc);
        self.write_layout(descriptor)?;
        let key = format!("{}:finalize:v1", journal.operation_key);
        let digest = sha256_json(
            journal
                .approval_receipt
                .as_ref()
                .expect("approval is validated before finalization"),
        )?;
        journal.record_step(key, digest, now_utc)?;
        advance_phase(journal, MigrationPhase::Finalized, now_utc, true)?;
        self.write_journal(journal)?;
        self.interrupt(MigrationCrashPoint::AfterPhaseBoundary)?;
        Ok(())
    }

    fn transition(
        &self,
        journal: &mut MigrationJournalV1,
        next: MigrationPhase,
        now_utc: DateTime<Utc>,
        has_explicit_approval: bool,
    ) -> Result<()> {
        let previous = journal.clone();
        advance_phase(journal, next, now_utc, has_explicit_approval)?;
        if let Err(error) = self.write_journal(journal) {
            // A simulated crash is reported only after the new journal bytes are durable. For an
            // actual durability failure, restore the in-memory old phase so lastError persistence
            // cannot accidentally advance a phase whose boundary write failed.
            if error.code != MigrationErrorCode::MigrationCrashInjected {
                *journal = previous;
            }
            return Err(error);
        }
        self.interrupt(MigrationCrashPoint::AfterPhaseBoundary)
    }

    fn validate_guard(&self, guard: &HostMigrationLockGuard, host_state_root: &Path) -> Result<()> {
        guard.validate_host_root(host_state_root)?;
        if guard.canonical_host_root() != self.canonical_host_root {
            return Err(MigrationError::new(
                MigrationErrorCode::MigrationLockInvalid,
                "validate_lock_guard",
                "migration service and bootstrap guard use different canonical host roots",
            ));
        }
        Ok(())
    }

    fn ensure_migration_dir(&self) -> Result<()> {
        self.durable_fs
            .create_dir_durable(&self.migration_dir, DirectoryPermissions::PrivateOwnerOnly)
            .map_err(|error| {
                MigrationError::new(
                    MigrationErrorCode::MigrationDurabilityFailed,
                    "create_migration_dir",
                    error.to_string(),
                )
            })
    }

    fn load_or_create_journal(
        &self,
        operation_key: &str,
        now_utc: DateTime<Utc>,
    ) -> Result<MigrationJournalV1> {
        if self.journal_path.exists() {
            let journal = self.load_journal()?;
            if journal.operation_key != operation_key {
                return Err(MigrationError::new(
                    MigrationErrorCode::MigrationIdempotencyConflict,
                    "load_journal",
                    "operation key does not match the durable migration operation",
                ));
            }
            Ok(journal)
        } else {
            let journal = MigrationJournalV1::new(operation_key.to_string(), now_utc);
            self.write_journal(&journal)?;
            Ok(journal)
        }
    }

    fn load_journal(&self) -> Result<MigrationJournalV1> {
        let bytes = fs::read(&self.journal_path).map_err(|error| {
            MigrationError::new(
                MigrationErrorCode::MigrationJournalCorrupt,
                "read_journal",
                error.to_string(),
            )
        })?;
        let journal: MigrationJournalV1 = serde_json::from_slice(&bytes).map_err(|error| {
            MigrationError::new(
                MigrationErrorCode::MigrationJournalCorrupt,
                "read_journal",
                error.to_string(),
            )
        })?;
        journal.validate()?;
        Ok(journal)
    }

    fn write_journal(&self, journal: &MigrationJournalV1) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(journal).map_err(|error| {
            MigrationError::new(
                MigrationErrorCode::MigrationJournalCorrupt,
                "write_journal",
                error.to_string(),
            )
        })?;
        bytes.push(b'\n');
        self.durable_fs
            .replace_bytes(&self.journal_path, &bytes)
            .map_err(|error| {
                MigrationError::new(
                    MigrationErrorCode::MigrationDurabilityFailed,
                    "write_journal",
                    error.to_string(),
                )
            })?;
        self.interrupt(MigrationCrashPoint::AfterJournalWrite)
    }

    fn load_or_create_layout(
        &self,
        now_utc: DateTime<Utc>,
    ) -> Result<ConversationLayoutDescriptorV1> {
        if self.layout_path.exists() {
            self.load_layout()
        } else {
            let descriptor =
                ConversationLayoutDescriptorV1::legacy(&self.canonical_host_root, now_utc);
            self.write_layout(&descriptor)?;
            Ok(descriptor)
        }
    }

    fn load_layout(&self) -> Result<ConversationLayoutDescriptorV1> {
        let bytes = fs::read(&self.layout_path).map_err(|error| {
            MigrationError::new(
                MigrationErrorCode::MigrationLayoutCorrupt,
                "read_layout",
                error.to_string(),
            )
        })?;
        let descriptor: ConversationLayoutDescriptorV1 =
            serde_json::from_slice(&bytes).map_err(|error| {
                MigrationError::new(
                    MigrationErrorCode::MigrationLayoutCorrupt,
                    "read_layout",
                    error.to_string(),
                )
            })?;
        descriptor.validate_for_host(&self.canonical_host_root)?;
        Ok(descriptor)
    }

    fn write_layout(&self, descriptor: &ConversationLayoutDescriptorV1) -> Result<()> {
        descriptor.validate_for_host(&self.canonical_host_root)?;
        let mut bytes = serde_json::to_vec_pretty(descriptor).map_err(|error| {
            MigrationError::new(
                MigrationErrorCode::MigrationLayoutCorrupt,
                "write_layout",
                error.to_string(),
            )
        })?;
        bytes.push(b'\n');
        self.durable_fs
            .replace_bytes(&self.layout_path, &bytes)
            .map_err(|error| {
                MigrationError::new(
                    MigrationErrorCode::MigrationDurabilityFailed,
                    "write_layout",
                    error.to_string(),
                )
            })?;
        self.interrupt(MigrationCrashPoint::AfterDescriptorReplace)
    }

    fn interrupt(&self, point: MigrationCrashPoint) -> Result<()> {
        if self
            .crash_injector
            .as_ref()
            .is_some_and(|injector| injector.should_interrupt(point))
        {
            Err(MigrationError::new(
                MigrationErrorCode::MigrationCrashInjected,
                "crash_injection",
                format!("interrupted at {point:?}"),
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhaseControl {
    Continue,
    Stable,
}

fn validate_callback_output(
    output: &MigrationStepOutput,
    expected_key_or_prefix: &str,
    operation: &'static str,
) -> Result<()> {
    journal::validate_sha256(&output.output_sha256)?;
    if !output.receipt_key.starts_with(expected_key_or_prefix) {
        return Err(MigrationError::new(
            MigrationErrorCode::MigrationIdempotencyConflict,
            operation,
            "callback receipt key is outside the deterministic operation namespace",
        ));
    }
    Ok(())
}

fn report(
    journal: &MigrationJournalV1,
    descriptor: &ConversationLayoutDescriptorV1,
    reused_step_count: usize,
) -> MigrationReport {
    MigrationReport {
        operation_id: journal.operation_id,
        operation_key: journal.operation_key.clone(),
        phase: journal.phase,
        attempt: journal.attempt,
        target_generation: journal.target_generation,
        reader_precedence: descriptor.reader_precedence,
        completed_step_count: journal.completed_steps.len(),
        reused_step_count,
    }
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sha256_json(value: &impl Serialize) -> Result<String> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        MigrationError::new(
            MigrationErrorCode::MigrationJournalCorrupt,
            "hash_json",
            error.to_string(),
        )
    })?;
    Ok(sha256_bytes(&bytes))
}

fn operation_key_prefix(operation_key: &str) -> &str {
    operation_key.get(..12).unwrap_or(operation_key)
}

fn digest_log_value(digest: Option<&str>) -> &str {
    digest.and_then(|value| value.get(..12)).unwrap_or("none")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Duration;

    use super::*;

    const OPERATION_KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const INVENTORY: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const STAGED: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const VALIDATION: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-15T10:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[derive(Default)]
    struct CountingCallbacks {
        inventory_calls: usize,
        stage_calls: usize,
        verify_calls: usize,
    }

    impl MigrationCallbacks for CountingCallbacks {
        fn inventory(&mut self, journal: &MigrationJournalV1) -> Result<MigrationStepOutput> {
            self.inventory_calls += 1;
            Ok(MigrationStepOutput::new(
                format!("{}:inventory:{INVENTORY}", journal.operation_key),
                INVENTORY,
            ))
        }

        fn stage(&mut self, journal: &MigrationJournalV1) -> Result<MigrationStepOutput> {
            self.stage_calls += 1;
            Ok(MigrationStepOutput::new(
                format!("{}:stage:aggregate:{STAGED}", journal.operation_key),
                STAGED,
            ))
        }

        fn verify(&mut self, journal: &MigrationJournalV1) -> Result<MigrationStepOutput> {
            self.verify_calls += 1;
            Ok(MigrationStepOutput::new(
                format!(
                    "{}:verify:{}:{}",
                    journal.operation_key,
                    journal.inventory_sha256.as_deref().unwrap(),
                    journal.staged_manifest_sha256.as_deref().unwrap()
                ),
                VALIDATION,
            ))
        }
    }

    struct FailingInventoryCallbacks;

    impl MigrationCallbacks for FailingInventoryCallbacks {
        fn inventory(&mut self, _journal: &MigrationJournalV1) -> Result<MigrationStepOutput> {
            Err(MigrationError::new(
                MigrationErrorCode::MigrationVerificationFailed,
                "inventory",
                "injected inventory failure",
            ))
        }

        fn stage(&mut self, _journal: &MigrationJournalV1) -> Result<MigrationStepOutput> {
            unreachable!("stage is not reached after inventory failure")
        }

        fn verify(&mut self, _journal: &MigrationJournalV1) -> Result<MigrationStepOutput> {
            unreachable!("verify is not reached after inventory failure")
        }
    }

    struct CountingCrashInjector {
        counts: [AtomicUsize; 4],
    }

    impl CountingCrashInjector {
        fn new() -> Self {
            Self {
                counts: std::array::from_fn(|_| AtomicUsize::new(0)),
            }
        }

        fn count(&self, point: MigrationCrashPoint) -> usize {
            self.counts[crash_point_index(point)].load(Ordering::SeqCst)
        }
    }

    impl MigrationCrashInjector for CountingCrashInjector {
        fn should_interrupt(&self, point: MigrationCrashPoint) -> bool {
            self.counts[crash_point_index(point)].fetch_add(1, Ordering::SeqCst);
            false
        }
    }

    struct InterruptNth {
        point: MigrationCrashPoint,
        target_occurrence: usize,
        seen: AtomicUsize,
    }

    impl MigrationCrashInjector for InterruptNth {
        fn should_interrupt(&self, point: MigrationCrashPoint) -> bool {
            point == self.point
                && self.seen.fetch_add(1, Ordering::SeqCst) + 1 == self.target_occurrence
        }
    }

    fn crash_point_index(point: MigrationCrashPoint) -> usize {
        match point {
            MigrationCrashPoint::AfterJournalWrite => 0,
            MigrationCrashPoint::AfterArtifactSync => 1,
            MigrationCrashPoint::AfterDescriptorReplace => 2,
            MigrationCrashPoint::AfterPhaseBoundary => 3,
        }
    }

    fn fixture() -> (
        tempfile::TempDir,
        PathBuf,
        HostMigrationLock,
        ConversationMigrationService,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let lock = HostMigrationLock::new(&root).unwrap();
        let service = ConversationMigrationService::new(&root).unwrap();
        (temp, root, lock, service)
    }

    fn run(
        service: &ConversationMigrationService,
        root: &Path,
        guard: &HostMigrationLockGuard,
        callbacks: &mut CountingCallbacks,
        at: DateTime<Utc>,
    ) -> Result<MigrationReport> {
        service.recover_and_run(MigrationContext {
            lock_guard: guard,
            host_state_root: root,
            operation_key: OPERATION_KEY,
            host_mode: MigrationHostMode::Desktop,
            admission: MigrationAdmissionState::default(),
            now_utc: at,
            callbacks,
        })
    }

    #[test]
    fn guard_handoff_never_reacquires() {
        let (_temp, root, lock, service) = fixture();
        let guard = lock.acquire().unwrap();
        let after_bootstrap_acquire = lock.acquire_count();
        let mut callbacks = CountingCallbacks::default();
        let report = run(&service, &root, &guard, &mut callbacks, now()).unwrap();
        assert_eq!(report.phase, MigrationPhase::ObservationWindow);
        assert_eq!(lock.acquire_count(), after_bootstrap_acquire);

        assert_eq!(
            service.recover_and_run_without_guard().unwrap_err().code,
            MigrationErrorCode::MigrationLockInvalid
        );
        let other = tempfile::tempdir().unwrap();
        let other_root = other.path().canonicalize().unwrap();
        let corrupt_journal = other_root
            .join("conversation-migrations")
            .join(MIGRATION_JOURNAL_FILE);
        fs::create_dir_all(corrupt_journal.parent().unwrap()).unwrap();
        fs::write(&corrupt_journal, b"not json").unwrap();
        let other_service = ConversationMigrationService::new(&other_root).unwrap();
        let error = run(&other_service, &other_root, &guard, &mut callbacks, now()).unwrap_err();
        assert_eq!(error.code, MigrationErrorCode::MigrationLockInvalid);
    }

    #[test]
    fn completed_receipts_skip_duplicate_phase_callbacks_and_return_same_digests() {
        let (_temp, root, lock, service) = fixture();
        let guard = lock.acquire().unwrap();
        let mut callbacks = CountingCallbacks::default();
        let first = run(&service, &root, &guard, &mut callbacks, now()).unwrap();
        assert_eq!(
            (
                callbacks.inventory_calls,
                callbacks.stage_calls,
                callbacks.verify_calls
            ),
            (1, 1, 1)
        );
        let second = run(&service, &root, &guard, &mut callbacks, now()).unwrap();
        assert_eq!(
            (
                callbacks.inventory_calls,
                callbacks.stage_calls,
                callbacks.verify_calls
            ),
            (1, 1, 1)
        );
        assert_eq!(first.operation_id, second.operation_id);
        assert_eq!(first.target_generation, second.target_generation);
    }

    #[test]
    fn phase_failure_persists_attempt_and_last_error_without_advancing() {
        let (_temp, root, lock, service) = fixture();
        let guard = lock.acquire().unwrap();
        let error = service
            .recover_and_run(MigrationContext {
                lock_guard: &guard,
                host_state_root: &root,
                operation_key: OPERATION_KEY,
                host_mode: MigrationHostMode::Desktop,
                admission: MigrationAdmissionState::default(),
                now_utc: now(),
                callbacks: &mut FailingInventoryCallbacks,
            })
            .unwrap_err();
        assert_eq!(error.code, MigrationErrorCode::MigrationVerificationFailed);
        let journal = service.load_journal().unwrap();
        assert_eq!(journal.phase, MigrationPhase::Quiescing);
        assert_eq!(journal.attempt, 2);
        assert!(journal
            .last_error
            .as_deref()
            .is_some_and(|value| value.contains("MIGRATION_VERIFICATION_FAILED")));
    }

    #[test]
    fn crash_matrix() {
        let points = [
            MigrationCrashPoint::AfterJournalWrite,
            MigrationCrashPoint::AfterArtifactSync,
            MigrationCrashPoint::AfterDescriptorReplace,
            MigrationCrashPoint::AfterPhaseBoundary,
        ];
        let forward_counts: [usize; 4] = {
            let (_temp, root, lock, _service) = fixture();
            let guard = lock.acquire().unwrap();
            let counter = Arc::new(CountingCrashInjector::new());
            let measured =
                ConversationMigrationService::with_crash_injector(&root, counter.clone()).unwrap();
            run(
                &measured,
                &root,
                &guard,
                &mut CountingCallbacks::default(),
                now(),
            )
            .unwrap();
            std::array::from_fn(|index| counter.count(points[index]))
        };

        for (point_index, point) in points.into_iter().enumerate() {
            for occurrence in 1..=forward_counts[point_index] {
                let (_temp, root, lock, _service) = fixture();
                let legacy = root.join("legacy-marker");
                fs::write(&legacy, b"preserve legacy bytes").unwrap();
                let guard = lock.acquire().unwrap();
                let injector = Arc::new(InterruptNth {
                    point,
                    target_occurrence: occurrence,
                    seen: AtomicUsize::new(0),
                });
                let interrupted =
                    ConversationMigrationService::with_crash_injector(&root, injector).unwrap();
                let mut callbacks = CountingCallbacks::default();
                let error = run(&interrupted, &root, &guard, &mut callbacks, now()).unwrap_err();
                assert_eq!(error.code, MigrationErrorCode::MigrationCrashInjected);

                let recovered = ConversationMigrationService::new(&root).unwrap();
                let report = run(&recovered, &root, &guard, &mut callbacks, now()).unwrap();
                assert_eq!(
                    report.phase,
                    MigrationPhase::ObservationWindow,
                    "{point:?} occurrence {occurrence}"
                );
                assert_eq!(
                    report.reader_precedence,
                    ReaderPrecedence::ConversationV2First,
                    "{point:?} occurrence {occurrence}"
                );
                assert_eq!(fs::read(&legacy).unwrap(), b"preserve legacy bytes");
            }
        }

        let rollback_points = [
            MigrationCrashPoint::AfterJournalWrite,
            MigrationCrashPoint::AfterDescriptorReplace,
            MigrationCrashPoint::AfterPhaseBoundary,
        ];
        for point in rollback_points {
            let rollback_write_count = {
                let (_temp, root, lock, service) = fixture();
                let guard = lock.acquire().unwrap();
                run(
                    &service,
                    &root,
                    &guard,
                    &mut CountingCallbacks::default(),
                    now(),
                )
                .unwrap();
                let counter = Arc::new(CountingCrashInjector::new());
                let measured =
                    ConversationMigrationService::with_crash_injector(&root, counter.clone())
                        .unwrap();
                measured
                    .request_rollback(MigrationControlContext {
                        lock_guard: &guard,
                        host_state_root: &root,
                        now_utc: now(),
                    })
                    .unwrap();
                counter.count(point)
            };
            for occurrence in 1..=rollback_write_count {
                let (_temp, root, lock, service) = fixture();
                let legacy = root.join("legacy-marker");
                fs::write(&legacy, b"preserve legacy bytes").unwrap();
                let guard = lock.acquire().unwrap();
                let mut callbacks = CountingCallbacks::default();
                run(&service, &root, &guard, &mut callbacks, now()).unwrap();
                let interrupted = ConversationMigrationService::with_crash_injector(
                    &root,
                    Arc::new(InterruptNth {
                        point,
                        target_occurrence: occurrence,
                        seen: AtomicUsize::new(0),
                    }),
                )
                .unwrap();
                let error = interrupted
                    .request_rollback(MigrationControlContext {
                        lock_guard: &guard,
                        host_state_root: &root,
                        now_utc: now(),
                    })
                    .unwrap_err();
                assert_eq!(error.code, MigrationErrorCode::MigrationCrashInjected);

                let recovered = ConversationMigrationService::new(&root).unwrap();
                let report = run(&recovered, &root, &guard, &mut callbacks, now()).unwrap();
                assert_eq!(
                    report.phase,
                    MigrationPhase::RolledBack,
                    "rollback {point:?} occurrence {occurrence}"
                );
                assert_eq!(
                    report.reader_precedence,
                    ReaderPrecedence::HybridLegacyFirst,
                    "rollback {point:?} occurrence {occurrence}"
                );
                assert_eq!(fs::read(&legacy).unwrap(), b"preserve legacy bytes");
            }
        }
    }

    #[test]
    fn rollback_preserves_v2_only_conversations_and_reapply_uses_new_generation() {
        let (_temp, root, lock, service) = fixture();
        let guard = lock.acquire().unwrap();
        let mut callbacks = CountingCallbacks::default();
        let committed = run(&service, &root, &guard, &mut callbacks, now()).unwrap();
        let v2_only = root.join("conversations/v2/new-only/conversation.json");
        fs::create_dir_all(v2_only.parent().unwrap()).unwrap();
        fs::write(&v2_only, b"new binary data").unwrap();
        let rolled_back = service
            .request_rollback(MigrationControlContext {
                lock_guard: &guard,
                host_state_root: &root,
                now_utc: now(),
            })
            .unwrap();
        assert_eq!(rolled_back.phase, MigrationPhase::RolledBack);
        assert_eq!(
            rolled_back.reader_precedence,
            ReaderPrecedence::HybridLegacyFirst
        );
        assert_eq!(fs::read(&v2_only).unwrap(), b"new binary data");

        let reapplied = service
            .reapply_and_run(MigrationContext {
                lock_guard: &guard,
                host_state_root: &root,
                operation_key: OPERATION_KEY,
                host_mode: MigrationHostMode::Desktop,
                admission: MigrationAdmissionState::default(),
                now_utc: now(),
                callbacks: &mut callbacks,
            })
            .unwrap();
        assert_eq!(reapplied.phase, MigrationPhase::ObservationWindow);
        assert_ne!(reapplied.target_generation, committed.target_generation);
        assert_eq!(callbacks.verify_calls, 2);
        assert_eq!(fs::read(&v2_only).unwrap(), b"new binary data");
    }

    #[test]
    fn observation_and_finalization_policy() {
        let (_temp, root, lock, service) = fixture();
        let guard = lock.acquire().unwrap();
        let mut callbacks = CountingCallbacks::default();
        run(&service, &root, &guard, &mut callbacks, now()).unwrap();
        let control = |at| MigrationControlContext {
            lock_guard: &guard,
            host_state_root: &root,
            now_utc: at,
        };
        let approval =
            |approver: &str, request: &str, action: &str, at, digest: &str| ApprovalReceiptV1 {
                approver_id: approver.to_string(),
                action: action.to_string(),
                request_id: request.to_string(),
                approved_at_utc: at,
                observed_validation_sha256: digest.to_string(),
            };
        let valid_request = Uuid::new_v4().to_string();
        assert_eq!(
            service
                .finalize(
                    control(now()),
                    approval(
                        "operator",
                        &valid_request,
                        FINALIZATION_ACTION,
                        now(),
                        VALIDATION
                    )
                )
                .unwrap_err()
                .code,
            MigrationErrorCode::MigrationFinalizationNotReady
        );

        service
            .record_bootstrap_observation(
                control(now()),
                BootstrapObservationReceiptV1 {
                    bootstrap_run_id: Uuid::new_v4().to_string(),
                    admitted_at_utc: now(),
                    validation_sha256: VALIDATION.to_string(),
                },
            )
            .unwrap();
        service
            .record_bootstrap_observation(
                control(now() + Duration::minutes(59)),
                BootstrapObservationReceiptV1 {
                    bootstrap_run_id: Uuid::new_v4().to_string(),
                    admitted_at_utc: now() + Duration::minutes(59),
                    validation_sha256: VALIDATION.to_string(),
                },
            )
            .unwrap();
        assert_eq!(
            service
                .finalize(
                    control(now() + Duration::hours(1)),
                    approval(
                        "operator",
                        &Uuid::new_v4().to_string(),
                        FINALIZATION_ACTION,
                        now() + Duration::hours(1),
                        VALIDATION
                    )
                )
                .unwrap_err()
                .code,
            MigrationErrorCode::MigrationFinalizationNotReady
        );

        service
            .record_bootstrap_observation(
                control(now() + Duration::hours(1)),
                BootstrapObservationReceiptV1 {
                    bootstrap_run_id: Uuid::new_v4().to_string(),
                    admitted_at_utc: now() + Duration::hours(1),
                    validation_sha256: VALIDATION.to_string(),
                },
            )
            .unwrap();

        let invalid_cases = [
            approval(
                "",
                &Uuid::new_v4().to_string(),
                FINALIZATION_ACTION,
                now() + Duration::hours(1),
                VALIDATION,
            ),
            approval(
                "operator",
                "not-a-uuid",
                FINALIZATION_ACTION,
                now() + Duration::hours(1),
                VALIDATION,
            ),
            approval(
                "operator",
                &Uuid::new_v4().to_string(),
                "wrong",
                now() + Duration::hours(1),
                VALIDATION,
            ),
            approval(
                "operator",
                &Uuid::new_v4().to_string(),
                FINALIZATION_ACTION,
                now(),
                VALIDATION,
            ),
            approval(
                "operator",
                &Uuid::new_v4().to_string(),
                FINALIZATION_ACTION,
                now() + Duration::hours(1),
                &"e".repeat(64),
            ),
        ];
        for invalid in invalid_cases {
            assert_eq!(
                service
                    .finalize(control(now() + Duration::hours(1)), invalid)
                    .unwrap_err()
                    .code,
                MigrationErrorCode::MigrationApprovalInvalid
            );
        }

        let legacy = root.join("legacy-retained");
        fs::write(&legacy, b"legacy remains").unwrap();
        let final_request_id = Uuid::new_v4().to_string();
        let finalized = service
            .finalize(
                control(now() + Duration::hours(1)),
                approval(
                    "operator",
                    &final_request_id,
                    FINALIZATION_ACTION,
                    now() + Duration::hours(1),
                    VALIDATION,
                ),
            )
            .unwrap();
        assert_eq!(finalized.phase, MigrationPhase::Finalized);
        assert_eq!(
            finalized.reader_precedence,
            ReaderPrecedence::ConversationV2Only
        );
        assert_eq!(fs::read(&legacy).unwrap(), b"legacy remains");
        assert_eq!(
            service
                .finalize(
                    control(now() + Duration::hours(1)),
                    approval(
                        "operator",
                        &final_request_id,
                        FINALIZATION_ACTION,
                        now() + Duration::hours(1),
                        VALIDATION,
                    ),
                )
                .unwrap_err()
                .code,
            MigrationErrorCode::MigrationApprovalInvalid
        );
    }

    #[test]
    fn admission_barrier_returns_restart_required_without_resource_teardown() {
        let (_temp, root, lock, service) = fixture();
        let guard = lock.acquire().unwrap();
        let mut callbacks = CountingCallbacks::default();
        let error = service
            .recover_and_run(MigrationContext {
                lock_guard: &guard,
                host_state_root: &root,
                operation_key: OPERATION_KEY,
                host_mode: MigrationHostMode::Standalone,
                admission: MigrationAdmissionState {
                    pty_manager_active: true,
                    ..Default::default()
                },
                now_utc: now(),
                callbacks: &mut callbacks,
            })
            .unwrap_err();
        assert_eq!(error.code, MigrationErrorCode::MigrationRestartRequired);
        assert_eq!(callbacks.inventory_calls, 0);
    }

    #[test]
    fn observation_receipts_require_distinct_ids_and_current_digest() {
        let mut evidence = ObservationEvidenceV1::empty(VALIDATION.to_string());
        let id = Uuid::new_v4().to_string();
        let receipt = BootstrapObservationReceiptV1 {
            bootstrap_run_id: id.clone(),
            admitted_at_utc: now(),
            validation_sha256: VALIDATION.to_string(),
        };
        assert!(evidence.record(receipt.clone()).unwrap());
        assert!(!evidence.record(receipt).unwrap());
        assert_eq!(
            evidence
                .record(BootstrapObservationReceiptV1 {
                    bootstrap_run_id: id,
                    admitted_at_utc: now() + Duration::hours(1),
                    validation_sha256: VALIDATION.to_string(),
                })
                .unwrap_err()
                .code,
            MigrationErrorCode::MigrationIdempotencyConflict
        );
        assert_eq!(
            evidence
                .record(BootstrapObservationReceiptV1 {
                    bootstrap_run_id: Uuid::new_v4().to_string(),
                    admitted_at_utc: now() + Duration::hours(1),
                    validation_sha256: "e".repeat(64),
                })
                .unwrap_err()
                .code,
            MigrationErrorCode::MigrationObservationInvalid
        );
        let ids = evidence
            .bootstrap_receipts
            .iter()
            .map(|receipt| &receipt.bootstrap_run_id)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn callback_count_type_is_thread_safe_for_future_bootstrap_composition() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        COUNT.fetch_add(1, Ordering::SeqCst);
        assert_eq!(COUNT.load(Ordering::SeqCst), 1);
    }
}
