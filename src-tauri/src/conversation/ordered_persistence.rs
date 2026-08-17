//! Globally bounded, source-sequenced Conversation persistence.
//!
//! Opaque ACP session bindings are resolved before admission. Accepted records are charged against
//! per-session, process-wide record, and serialized-byte budgets, then routed to one of eight
//! shared Tokio workers. A barrier uses one absolute deadline for control delivery,
//! acknowledgement, adapter flush, and worker join.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant as StdInstant};

use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::conversation::{
    ConversationId, ConversationPersistenceAdapter, ConversationPersistenceError,
};

/// Fixed number of shared ordered writer tasks.
pub const WRITER_SHARDS: usize = 8;
/// Maximum accepted-but-not-yet-persisted records for one opaque session.
pub const PER_SESSION_PENDING_RECORDS: usize = 256;
/// Maximum accepted-but-not-yet-persisted records across the coordinator.
pub const GLOBAL_PENDING_RECORDS: usize = 4096;
/// Maximum serialized bytes held by accepted records across the coordinator.
pub const GLOBAL_PENDING_BYTES: usize = 16 * 1024 * 1024;
/// Default absolute budget for a complete flush or shutdown attempt.
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
/// Compatibility alias retained for existing callers.
pub const QUEUE_CAPACITY: usize = PER_SESSION_PENDING_RECORDS;

// One barrier may sit behind every admitted record in a single shard.
const SHARD_CHANNEL_CAPACITY: usize = GLOBAL_PENDING_RECORDS + 1;

const SOURCE_SEQUENCE_INVALID: &str = "CONVERSATION_SOURCE_SEQUENCE_INVALID";
const WRITER_UNHEALTHY: &str = "CONVERSATION_PERSISTENCE_UNHEALTHY";
const WRITER_QUEUE_CLOSED: &str = "CONVERSATION_PERSISTENCE_QUEUE_CLOSED";
const WRITER_QUEUE_SATURATED: &str = "CONVERSATION_PERSISTENCE_QUEUE_SATURATED";
const WRITER_BYTES_SATURATED: &str = "CONVERSATION_PERSISTENCE_BYTES_SATURATED";
const WRITER_RECORD_TOO_LARGE: &str = "CONVERSATION_PERSISTENCE_RECORD_TOO_LARGE";
const WRITER_SERIALIZATION_FAILED: &str = "CONVERSATION_PERSISTENCE_SERIALIZATION_FAILED";
const WRITER_DRAIN_TIMEOUT: &str = "CONVERSATION_PERSISTENCE_DRAIN_TIMEOUT";
const WRITER_FRONTIER_MISMATCH: &str = "CONVERSATION_PERSISTENCE_FRONTIER_MISMATCH";
const WRITER_SHUT_DOWN: &str = "CONVERSATION_PERSISTENCE_SHUT_DOWN";

/// Secret-safe health snapshot for one mapped session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedPersistenceHealth {
    pub conversation_id: ConversationId,
    pub pending_count: usize,
    pub pending_bytes: usize,
    pub last_accepted_source_seq: u64,
    pub last_persisted_source_seq: u64,
    pub last_error_code: Option<&'static str>,
    pub running: bool,
}

/// Secret-safe quantitative coordinator snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderedPersistenceMetrics {
    pub active_writer_tasks: usize,
    pub retained_sessions: usize,
    pub pending_records: usize,
    pub pending_bytes: usize,
    pub max_pending_records: usize,
    pub max_pending_bytes: usize,
    pub max_per_session_pending_records: usize,
}

type AppendFuture<'a> = Pin<
    Box<dyn Future<Output = std::result::Result<u64, ConversationPersistenceError>> + Send + 'a>,
>;
type FlushFuture<'a> = Pin<
    Box<dyn Future<Output = std::result::Result<(), ConversationPersistenceError>> + Send + 'a>,
>;

trait PersistenceTarget: Send + Sync + 'static {
    fn resolve(&self, agent_session_id: &str) -> Option<ConversationId>;

    fn append<'a>(
        &'a self,
        agent_session_id: &'a str,
        event_type: &'a str,
        payload: Value,
    ) -> AppendFuture<'a>;

    fn flush(&self) -> FlushFuture<'_>;
}

struct AdapterTarget {
    adapter: Arc<ConversationPersistenceAdapter>,
}

impl PersistenceTarget for AdapterTarget {
    fn resolve(&self, agent_session_id: &str) -> Option<ConversationId> {
        self.adapter.conversation_id_for_session(agent_session_id)
    }

    fn append<'a>(
        &'a self,
        agent_session_id: &'a str,
        event_type: &'a str,
        payload: Value,
    ) -> AppendFuture<'a> {
        Box::pin(
            self.adapter
                .append_acp_event(agent_session_id, event_type, payload),
        )
    }

    fn flush(&self) -> FlushFuture<'_> {
        Box::pin(self.adapter.flush_all())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SerializedRecordCharge<'a> {
    event_type: &'a str,
    payload: &'a Value,
}

struct SessionState {
    conversation_id: ConversationId,
    shard: usize,
    pending_records: usize,
    pending_bytes: usize,
    accepted_frontier: u64,
    persisted_frontier: u64,
    last_error_code: Option<&'static str>,
}

#[derive(Default)]
struct CoordinatorState {
    sessions: HashMap<String, SessionState>,
    pending_records: usize,
    pending_bytes: usize,
    max_pending_records: usize,
    max_pending_bytes: usize,
    max_per_session_pending_records: usize,
    barrier_active: bool,
}

struct CoordinatorShared {
    target: Arc<dyn PersistenceTarget>,
    state: Mutex<CoordinatorState>,
    capacity_available: Condvar,
    shutting_down: AtomicBool,
    active_writer_tasks: AtomicUsize,
}

impl CoordinatorShared {
    fn metrics(&self) -> OrderedPersistenceMetrics {
        let state = self.state.lock();
        OrderedPersistenceMetrics {
            active_writer_tasks: self.active_writer_tasks.load(Ordering::Acquire),
            retained_sessions: state.sessions.len(),
            pending_records: state.pending_records,
            pending_bytes: state.pending_bytes,
            max_pending_records: state.max_pending_records,
            max_pending_bytes: state.max_pending_bytes,
            max_per_session_pending_records: state.max_per_session_pending_records,
        }
    }

    fn fatal_session_error(&self, session_key: &str) -> Option<&'static str> {
        self.state
            .lock()
            .sessions
            .get(session_key)
            .and_then(|session| session.last_error_code)
            .filter(|code| *code != SOURCE_SEQUENCE_INVALID)
    }

    fn finish_record(
        &self,
        session_key: &str,
        source_seq: u64,
        charged_bytes: usize,
        result: std::result::Result<(), &'static str>,
    ) {
        let mut state = self.state.lock();
        state.pending_records = state.pending_records.saturating_sub(1);
        state.pending_bytes = state.pending_bytes.saturating_sub(charged_bytes);
        if let Some(session) = state.sessions.get_mut(session_key) {
            session.pending_records = session.pending_records.saturating_sub(1);
            session.pending_bytes = session.pending_bytes.saturating_sub(charged_bytes);
            match result {
                Ok(()) if source_seq > session.persisted_frontier => {
                    session.persisted_frontier = source_seq;
                }
                Ok(()) => {
                    session.last_error_code = Some(WRITER_FRONTIER_MISMATCH);
                }
                Err(code) => {
                    session.last_error_code.get_or_insert(code);
                }
            }
        }
        drop(state);
        self.capacity_available.notify_all();
    }

    fn observe_barrier(&self, targets: &[BarrierTarget]) -> BarrierAck {
        let state = self.state.lock();
        let mut error_code = None;
        for target in targets {
            match state.sessions.get(&target.session_key) {
                Some(session) if session.last_error_code.is_some() => {
                    error_code.get_or_insert(WRITER_UNHEALTHY);
                }
                Some(session) if session.persisted_frontier >= target.target_source_seq => {}
                _ => {
                    error_code.get_or_insert(WRITER_FRONTIER_MISMATCH);
                }
            }
        }
        BarrierAck {
            observed: targets.to_vec(),
            error_code,
        }
    }

    fn reap_observed(&self, observed: &[BarrierTarget]) {
        let mut state = self.state.lock();
        for target in observed {
            let removable = state
                .sessions
                .get(&target.session_key)
                .is_some_and(|session| {
                    session.last_error_code.is_none()
                        && session.pending_records == 0
                        && session.pending_bytes == 0
                        && session.accepted_frontier == target.target_source_seq
                        && session.persisted_frontier == target.target_source_seq
                });
            if removable {
                state.sessions.remove(&target.session_key);
            }
        }
    }
}

struct AdmissionPermit {
    shared: Arc<CoordinatorShared>,
    session_key: String,
    source_seq: u64,
    charged_bytes: usize,
    released: bool,
}

impl AdmissionPermit {
    fn complete(mut self, result: std::result::Result<(), &'static str>) {
        self.released = true;
        self.shared.finish_record(
            &self.session_key,
            self.source_seq,
            self.charged_bytes,
            result,
        );
    }

    fn disarm(&mut self) {
        self.released = true;
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        if !self.released {
            self.shared.finish_record(
                &self.session_key,
                self.source_seq,
                self.charged_bytes,
                Err(WRITER_QUEUE_CLOSED),
            );
        }
    }
}

struct RecordCommand {
    agent_session_id: String,
    event_type: String,
    payload: Value,
    permit: AdmissionPermit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BarrierTarget {
    session_key: String,
    target_source_seq: u64,
}

struct BarrierAck {
    observed: Vec<BarrierTarget>,
    error_code: Option<&'static str>,
}

enum WorkerCommand {
    Record(RecordCommand),
    Barrier {
        targets: Vec<BarrierTarget>,
        reply: oneshot::Sender<BarrierAck>,
    },
    Shutdown,
}

struct WorkerTaskGuard {
    shared: Arc<CoordinatorShared>,
}

impl Drop for WorkerTaskGuard {
    fn drop(&mut self) {
        self.shared
            .active_writer_tasks
            .fetch_sub(1, Ordering::AcqRel);
        self.shared.capacity_available.notify_all();
    }
}

struct ShardControl {
    sender: mpsc::Sender<WorkerCommand>,
    join_handle: Mutex<Option<JoinHandle<()>>>,
}

struct SubmissionPause {
    shared: Arc<CoordinatorShared>,
    active: bool,
}

impl SubmissionPause {
    fn begin(shared: Arc<CoordinatorShared>) -> (Self, Vec<Vec<BarrierTarget>>) {
        let mut state = shared.state.lock();
        state.barrier_active = true;
        let mut targets = (0..WRITER_SHARDS).map(|_| Vec::new()).collect::<Vec<_>>();
        for (session_key, session) in &state.sessions {
            targets[session.shard].push(BarrierTarget {
                session_key: session_key.clone(),
                target_source_seq: session.accepted_frontier,
            });
        }
        for shard_targets in &mut targets {
            shard_targets.sort_by(|left, right| left.session_key.cmp(&right.session_key));
        }
        drop(state);
        (
            Self {
                shared,
                active: true,
            },
            targets,
        )
    }

    fn resume(&mut self) {
        if self.active {
            self.shared.state.lock().barrier_active = false;
            self.active = false;
            self.shared.capacity_available.notify_all();
        }
    }
}

impl Drop for SubmissionPause {
    fn drop(&mut self) {
        self.resume();
    }
}

/// One globally bounded ordered persistence coordinator shared by every relay using an adapter.
pub struct OrderedConversationPersistence {
    shared: Arc<CoordinatorShared>,
    shards: Vec<ShardControl>,
    lifecycle_lock: tokio::sync::Mutex<()>,
    drain_timeout: Duration,
}

impl OrderedConversationPersistence {
    #[must_use]
    pub fn new(adapter: Arc<ConversationPersistenceAdapter>) -> Self {
        Self::with_target(Arc::new(AdapterTarget { adapter }), DEFAULT_DRAIN_TIMEOUT)
    }

    fn with_target(target: Arc<dyn PersistenceTarget>, drain_timeout: Duration) -> Self {
        Self::with_target_and_handle(target, drain_timeout, coordinator_runtime_handle())
    }

    fn with_target_and_handle(
        target: Arc<dyn PersistenceTarget>,
        drain_timeout: Duration,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let shared = Arc::new(CoordinatorShared {
            target,
            state: Mutex::new(CoordinatorState::default()),
            capacity_available: Condvar::new(),
            shutting_down: AtomicBool::new(false),
            active_writer_tasks: AtomicUsize::new(0),
        });
        let mut shards = Vec::with_capacity(WRITER_SHARDS);
        for shard_index in 0..WRITER_SHARDS {
            let (sender, receiver) = mpsc::channel(SHARD_CHANNEL_CAPACITY);
            shared.active_writer_tasks.fetch_add(1, Ordering::AcqRel);
            let guard = WorkerTaskGuard {
                shared: Arc::clone(&shared),
            };
            let join_handle =
                runtime.spawn(run_shard(shard_index, receiver, Arc::clone(&shared), guard));
            shards.push(ShardControl {
                sender,
                join_handle: Mutex::new(Some(join_handle)),
            });
        }
        log::info!(
            "[conversation-persistence] coordinator ready workers={} pending_records=0 pending_bytes=0 frontier=0 elapsed_ms=0",
            WRITER_SHARDS
        );
        Self {
            shared,
            shards,
            lifecycle_lock: tokio::sync::Mutex::new(()),
            drain_timeout,
        }
    }

    /// Accept one source-sequenced relay event for ordered persistence.
    ///
    /// Binding resolution and serialization occur before any session state or budget is allocated.
    /// Record-count saturation backpressures the synchronous producer without runtime polling;
    /// byte saturation rejects before queue acceptance.
    pub fn submit(
        &self,
        agent_session_id: &str,
        source_seq: u64,
        event_type: &str,
        payload: Value,
    ) -> Result<(), ConversationPersistenceError> {
        if self.shared.shutting_down.load(Ordering::Acquire) {
            return Err(persistence_error(
                WRITER_SHUT_DOWN,
                "ordered_submit",
                "ordered persistence is shutting down",
            ));
        }
        if source_seq == 0 {
            return Err(persistence_error(
                SOURCE_SEQUENCE_INVALID,
                "ordered_submit",
                "source sequence must be greater than zero",
            ));
        }

        let conversation_id = self
            .shared
            .target
            .resolve(agent_session_id)
            .ok_or_else(|| {
                persistence_error(
                    "CONVERSATION_BINDING_NOT_FOUND",
                    "ordered_submit",
                    "opaque agent session id has no canonical Conversation binding",
                )
            })?;
        let charged_bytes = serialized_record_bytes(event_type, &payload)?;
        if charged_bytes > GLOBAL_PENDING_BYTES {
            return Err(persistence_error(
                WRITER_RECORD_TOO_LARGE,
                "ordered_submit",
                format!(
                    "serialized record bytes {charged_bytes} exceed global byte limit {GLOBAL_PENDING_BYTES}"
                ),
            ));
        }

        let shard = shard_for(agent_session_id);
        let mut saturation_logged = false;
        let mut state = self.shared.state.lock();
        loop {
            if self.shared.shutting_down.load(Ordering::Acquire) {
                return Err(persistence_error(
                    WRITER_SHUT_DOWN,
                    "ordered_submit",
                    "ordered persistence is shutting down",
                ));
            }
            if state.barrier_active {
                self.shared.capacity_available.wait(&mut state);
                continue;
            }

            if let Some(session) = state.sessions.get(agent_session_id) {
                if session.conversation_id != conversation_id {
                    return Err(persistence_error(
                        "CONVERSATION_BINDING_CONFLICT",
                        "ordered_submit",
                        "agent session binding changed while ordered state was retained",
                    ));
                }
                if let Some(code) = session.last_error_code {
                    let reported_code = if code == SOURCE_SEQUENCE_INVALID {
                        SOURCE_SEQUENCE_INVALID
                    } else {
                        WRITER_UNHEALTHY
                    };
                    return Err(persistence_error(
                        reported_code,
                        "ordered_submit",
                        format!("ordered persistence circuit is open ({code})"),
                    ));
                }
                if source_seq <= session.accepted_frontier {
                    let accepted = session.accepted_frontier;
                    state
                        .sessions
                        .get_mut(agent_session_id)
                        .expect("session was read while state lock was held")
                        .last_error_code = Some(SOURCE_SEQUENCE_INVALID);
                    self.shared.capacity_available.notify_all();
                    log::error!(
                        "[conversation-persistence] frontier mismatch code={} pending_records={} pending_bytes={}",
                        SOURCE_SEQUENCE_INVALID,
                        state.pending_records,
                        state.pending_bytes
                    );
                    return Err(persistence_error(
                        SOURCE_SEQUENCE_INVALID,
                        "ordered_submit",
                        format!(
                            "source sequence must be strictly increasing; accepted frontier is {accepted}"
                        ),
                    ));
                }
            }

            let session_pending = state
                .sessions
                .get(agent_session_id)
                .map_or(0, |session| session.pending_records);
            if state.pending_bytes.saturating_add(charged_bytes) > GLOBAL_PENDING_BYTES {
                log::warn!(
                    "[conversation-persistence] admission saturated code={} pending_records={} pending_bytes={}",
                    WRITER_BYTES_SATURATED,
                    state.pending_records,
                    state.pending_bytes
                );
                return Err(persistence_error(
                    WRITER_BYTES_SATURATED,
                    "ordered_submit",
                    format!(
                        "serialized record bytes {charged_bytes} exceed remaining global byte budget {}",
                        GLOBAL_PENDING_BYTES.saturating_sub(state.pending_bytes)
                    ),
                ));
            }
            if session_pending >= PER_SESSION_PENDING_RECORDS
                || state.pending_records >= GLOBAL_PENDING_RECORDS
            {
                if !saturation_logged {
                    log::warn!(
                        "[conversation-persistence] admission saturated code={} pending_records={} pending_bytes={}",
                        WRITER_QUEUE_SATURATED,
                        state.pending_records,
                        state.pending_bytes
                    );
                    saturation_logged = true;
                }
                self.shared.capacity_available.wait(&mut state);
                continue;
            }

            let previous_frontier = state
                .sessions
                .get(agent_session_id)
                .map_or(0, |session| session.accepted_frontier);
            let newly_created = !state.sessions.contains_key(agent_session_id);
            state.pending_records += 1;
            state.pending_bytes += charged_bytes;
            state.max_pending_records = state.max_pending_records.max(state.pending_records);
            state.max_pending_bytes = state.max_pending_bytes.max(state.pending_bytes);
            let session_pending_after = {
                let session = state
                    .sessions
                    .entry(agent_session_id.to_string())
                    .or_insert(SessionState {
                        conversation_id,
                        shard,
                        pending_records: 0,
                        pending_bytes: 0,
                        accepted_frontier: 0,
                        persisted_frontier: 0,
                        last_error_code: None,
                    });
                session.pending_records += 1;
                session.pending_bytes += charged_bytes;
                session.accepted_frontier = source_seq;
                session.pending_records
            };
            state.max_per_session_pending_records = state
                .max_per_session_pending_records
                .max(session_pending_after);

            let permit = AdmissionPermit {
                shared: Arc::clone(&self.shared),
                session_key: agent_session_id.to_string(),
                source_seq,
                charged_bytes,
                released: false,
            };
            let command = WorkerCommand::Record(RecordCommand {
                agent_session_id: agent_session_id.to_string(),
                event_type: event_type.to_string(),
                payload,
                permit,
            });
            match self.shards[shard].sender.try_send(command) {
                Ok(()) => return Ok(()),
                Err(send_error) => {
                    let (mut command, code, detail) = match send_error {
                        mpsc::error::TrySendError::Full(command) => (
                            command,
                            WRITER_QUEUE_SATURATED,
                            "ordered shard queue is saturated",
                        ),
                        mpsc::error::TrySendError::Closed(command) => (
                            command,
                            WRITER_QUEUE_CLOSED,
                            "ordered shard queue is closed",
                        ),
                    };
                    disarm_record_command(&mut command);
                    rollback_admission(
                        &mut state,
                        agent_session_id,
                        previous_frontier,
                        charged_bytes,
                        newly_created,
                    );
                    self.shared.capacity_available.notify_all();
                    log::error!(
                        "[conversation-persistence] enqueue failed code={} pending_records={} pending_bytes={}",
                        code,
                        state.pending_records,
                        state.pending_bytes
                    );
                    return Err(persistence_error(code, "ordered_submit", detail));
                }
            }
        }
    }

    /// Health for a mapped session. No session state or byte budget is created by this query.
    pub fn health(
        &self,
        agent_session_id: &str,
    ) -> Result<Option<OrderedPersistenceHealth>, ConversationPersistenceError> {
        let conversation_id = self
            .shared
            .target
            .resolve(agent_session_id)
            .ok_or_else(|| {
                persistence_error(
                    "CONVERSATION_BINDING_NOT_FOUND",
                    "ordered_health",
                    "opaque agent session id has no canonical Conversation binding",
                )
            })?;
        let state = self.shared.state.lock();
        Ok(state.sessions.get(agent_session_id).and_then(|session| {
            (session.conversation_id == conversation_id).then_some(OrderedPersistenceHealth {
                conversation_id,
                pending_count: session.pending_records,
                pending_bytes: session.pending_bytes,
                last_accepted_source_seq: session.accepted_frontier,
                last_persisted_source_seq: session.persisted_frontier,
                last_error_code: session.last_error_code,
                running: !self.shared.shutting_down.load(Ordering::Acquire)
                    && self.shared.active_writer_tasks.load(Ordering::Acquire) > 0,
            })
        }))
    }

    /// Number of retained per-session frontier/circuit entries.
    #[must_use]
    pub fn retained_worker_count(&self) -> usize {
        self.shared.state.lock().sessions.len()
    }

    /// Number of fixed shared Tokio writer tasks that have not exited.
    #[must_use]
    pub fn active_worker_count(&self) -> usize {
        self.shared.active_writer_tasks.load(Ordering::Acquire)
    }

    /// Secret-safe current and high-water resource accounting.
    #[must_use]
    pub fn metrics(&self) -> OrderedPersistenceMetrics {
        self.shared.metrics()
    }

    /// Flush using the default single absolute deadline.
    pub async fn flush_all(&self) -> Result<(), ConversationPersistenceError> {
        self.flush_all_until(Instant::now() + self.drain_timeout)
            .await
    }

    /// Await every accepted frontier and the adapter flush under one absolute deadline.
    pub async fn flush_all_until(
        &self,
        deadline: Instant,
    ) -> Result<(), ConversationPersistenceError> {
        let started = StdInstant::now();
        let lifecycle_guard = tokio::time::timeout_at(deadline, self.lifecycle_lock.lock())
            .await
            .map_err(|_| deadline_error("ordered_flush_all", "timed out acquiring flush gate"))?;
        let result = self.flush_until_locked(deadline).await;
        drop(lifecycle_guard);
        self.log_boundary_result("flush", started, result.as_ref().err());
        result
    }

    /// Shutdown using the default single absolute deadline.
    pub async fn shutdown(&self) -> Result<(), ConversationPersistenceError> {
        self.shutdown_until(Instant::now() + self.drain_timeout)
            .await
    }

    /// Stop admission, flush, signal all shards, and join them under one absolute deadline.
    pub async fn shutdown_until(
        &self,
        deadline: Instant,
    ) -> Result<(), ConversationPersistenceError> {
        let started = StdInstant::now();
        let lifecycle_guard = tokio::time::timeout_at(deadline, self.lifecycle_lock.lock())
            .await
            .map_err(|_| deadline_error("ordered_shutdown", "timed out acquiring shutdown gate"))?;
        self.shared.shutting_down.store(true, Ordering::Release);
        self.shared.capacity_available.notify_all();

        let mut first_error = self.flush_until_locked(deadline).await.err();
        for shard in &self.shards {
            match tokio::time::timeout_at(deadline, shard.sender.send(WorkerCommand::Shutdown))
                .await
            {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    first_error.get_or_insert_with(|| {
                        persistence_error(
                            WRITER_QUEUE_CLOSED,
                            "ordered_shutdown",
                            "ordered shard closed before shutdown receipt",
                        )
                    });
                }
                Err(_) => {
                    first_error.get_or_insert_with(|| {
                        deadline_error(
                            "ordered_shutdown",
                            "timed out enqueueing ordered shard shutdown",
                        )
                    });
                }
            }
        }

        for shard in &self.shards {
            let Some(mut handle) = shard.join_handle.lock().take() else {
                continue;
            };
            match tokio::time::timeout_at(deadline, &mut handle).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    first_error.get_or_insert_with(|| {
                        persistence_error(
                            WRITER_QUEUE_CLOSED,
                            "ordered_shutdown",
                            "ordered shard join failed",
                        )
                    });
                }
                Err(_) => {
                    handle.abort();
                    first_error.get_or_insert_with(|| {
                        deadline_error(
                            "ordered_shutdown",
                            "timed out joining ordered persistence shards",
                        )
                    });
                }
            }
        }
        drop(lifecycle_guard);
        let result = first_error.map_or(Ok(()), Err);
        self.log_boundary_result("shutdown", started, result.as_ref().err());
        result
    }

    async fn flush_until_locked(
        &self,
        deadline: Instant,
    ) -> Result<(), ConversationPersistenceError> {
        let (mut submission_pause, targets_by_shard) =
            SubmissionPause::begin(Arc::clone(&self.shared));
        let mut acknowledgements = Vec::with_capacity(WRITER_SHARDS);
        let mut first_error = None;

        for (shard, targets) in self.shards.iter().zip(targets_by_shard) {
            let (reply, receiver) = oneshot::channel();
            let command = WorkerCommand::Barrier { targets, reply };
            match tokio::time::timeout_at(deadline, shard.sender.send(command)).await {
                Ok(Ok(())) => acknowledgements.push(receiver),
                Ok(Err(_)) => {
                    first_error.get_or_insert_with(|| {
                        persistence_error(
                            WRITER_QUEUE_CLOSED,
                            "ordered_flush_all",
                            "ordered shard closed before barrier enqueue",
                        )
                    });
                }
                Err(_) => {
                    first_error.get_or_insert_with(|| {
                        deadline_error(
                            "ordered_flush_all",
                            "timed out enqueueing ordered shard barrier",
                        )
                    });
                }
            }
        }
        submission_pause.resume();

        let mut observed = Vec::new();
        for receiver in acknowledgements {
            match tokio::time::timeout_at(deadline, receiver).await {
                Ok(Ok(acknowledgement)) => {
                    if let Some(code) = acknowledgement.error_code {
                        first_error.get_or_insert_with(|| {
                            persistence_error(
                                code,
                                "ordered_flush_all",
                                "ordered shard did not observe its accepted frontier",
                            )
                        });
                    } else {
                        observed.extend(acknowledgement.observed);
                    }
                }
                Ok(Err(_)) => {
                    first_error.get_or_insert_with(|| {
                        persistence_error(
                            WRITER_QUEUE_CLOSED,
                            "ordered_flush_all",
                            "ordered shard dropped its barrier acknowledgement",
                        )
                    });
                }
                Err(_) => {
                    first_error.get_or_insert_with(|| {
                        deadline_error(
                            "ordered_flush_all",
                            "timed out awaiting ordered shard barrier",
                        )
                    });
                }
            }
        }

        match tokio::time::timeout_at(deadline, self.shared.target.flush()).await {
            Ok(Ok(())) => {
                self.shared.reap_observed(&observed);
            }
            Ok(Err(error)) => {
                first_error.get_or_insert_with(|| {
                    persistence_error(
                        WRITER_UNHEALTHY,
                        "ordered_flush_all",
                        format!("persistence adapter flush failed ({})", error.code),
                    )
                });
            }
            Err(_) => {
                first_error.get_or_insert_with(|| {
                    deadline_error(
                        "ordered_flush_all",
                        "timed out awaiting persistence adapter flush",
                    )
                });
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    fn log_boundary_result(
        &self,
        operation: &'static str,
        started: StdInstant,
        error: Option<&ConversationPersistenceError>,
    ) {
        let metrics = self.metrics();
        let frontier = self
            .shared
            .state
            .lock()
            .sessions
            .values()
            .map(|session| session.persisted_frontier)
            .max()
            .unwrap_or(0);
        if let Some(error) = error {
            log::error!(
                "[conversation-persistence] boundary failed operation={} code={} workers={} pending_records={} pending_bytes={} frontier={} elapsed_ms={}",
                operation,
                error.code,
                metrics.active_writer_tasks,
                metrics.pending_records,
                metrics.pending_bytes,
                frontier,
                started.elapsed().as_millis()
            );
        } else {
            log::info!(
                "[conversation-persistence] boundary complete operation={} workers={} pending_records={} pending_bytes={} frontier={} elapsed_ms={}",
                operation,
                metrics.active_writer_tasks,
                metrics.pending_records,
                metrics.pending_bytes,
                frontier,
                started.elapsed().as_millis()
            );
        }
    }
}

impl Drop for OrderedConversationPersistence {
    fn drop(&mut self) {
        self.shared.shutting_down.store(true, Ordering::Release);
        self.shared.state.lock().barrier_active = false;
        self.shared.capacity_available.notify_all();
        for shard in &self.shards {
            let _ = shard.sender.try_send(WorkerCommand::Shutdown);
            if let Some(handle) = shard.join_handle.lock().take() {
                handle.abort();
            }
        }
        let metrics = self.metrics();
        if metrics.pending_records > 0 {
            log::error!(
                "[conversation-persistence] drop aborted pending work code={} workers={} pending_records={} pending_bytes={}",
                WRITER_QUEUE_CLOSED,
                metrics.active_writer_tasks,
                metrics.pending_records,
                metrics.pending_bytes
            );
        }
    }
}

async fn run_shard(
    _shard_index: usize,
    mut receiver: mpsc::Receiver<WorkerCommand>,
    shared: Arc<CoordinatorShared>,
    _guard: WorkerTaskGuard,
) {
    while let Some(command) = receiver.recv().await {
        match command {
            WorkerCommand::Record(record) => {
                let RecordCommand {
                    agent_session_id,
                    event_type,
                    payload,
                    permit,
                } = record;
                if let Some(code) = shared.fatal_session_error(&agent_session_id) {
                    permit.complete(Err(code));
                    continue;
                }
                match shared
                    .target
                    .append(&agent_session_id, &event_type, payload)
                    .await
                {
                    Ok(_) => permit.complete(Ok(())),
                    Err(error) => {
                        let code = error.code;
                        permit.complete(Err(code));
                        let metrics = shared.metrics();
                        log::error!(
                            "[conversation-persistence] append failed code={} workers={} pending_records={} pending_bytes={}",
                            code,
                            metrics.active_writer_tasks,
                            metrics.pending_records,
                            metrics.pending_bytes
                        );
                    }
                }
            }
            WorkerCommand::Barrier { targets, reply } => {
                let _ = reply.send(shared.observe_barrier(&targets));
            }
            WorkerCommand::Shutdown => break,
        }
    }
}

fn coordinator_runtime_handle() -> tokio::runtime::Handle {
    tokio::runtime::Handle::try_current()
        .unwrap_or_else(|_| tauri::async_runtime::handle().inner().clone())
}

fn shard_for(agent_session_id: &str) -> usize {
    let hash = agent_session_id
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    hash as usize % WRITER_SHARDS
}

fn serialized_record_bytes(
    event_type: &str,
    payload: &Value,
) -> Result<usize, ConversationPersistenceError> {
    serde_json::to_vec(&SerializedRecordCharge {
        event_type,
        payload,
    })
    .map(|bytes| bytes.len())
    .map_err(|_| {
        persistence_error(
            WRITER_SERIALIZATION_FAILED,
            "ordered_submit",
            "record could not be serialized for admission accounting",
        )
    })
}

fn disarm_record_command(command: &mut WorkerCommand) {
    if let WorkerCommand::Record(record) = command {
        record.permit.disarm();
    }
}

fn rollback_admission(
    state: &mut CoordinatorState,
    session_key: &str,
    previous_frontier: u64,
    charged_bytes: usize,
    newly_created: bool,
) {
    state.pending_records = state.pending_records.saturating_sub(1);
    state.pending_bytes = state.pending_bytes.saturating_sub(charged_bytes);
    if let Some(session) = state.sessions.get_mut(session_key) {
        session.pending_records = session.pending_records.saturating_sub(1);
        session.pending_bytes = session.pending_bytes.saturating_sub(charged_bytes);
        session.accepted_frontier = previous_frontier;
    }
    if newly_created
        && state
            .sessions
            .get(session_key)
            .is_some_and(|session| session.pending_records == 0)
    {
        state.sessions.remove(session_key);
    }
}

fn deadline_error(operation: &'static str, detail: &'static str) -> ConversationPersistenceError {
    persistence_error(WRITER_DRAIN_TIMEOUT, operation, detail)
}

fn persistence_error(
    code: &'static str,
    operation: &'static str,
    detail: impl Into<String>,
) -> ConversationPersistenceError {
    ConversationPersistenceError {
        code,
        operation,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use tokio::sync::Notify;

    type RecordedEvent = (u64, String, Value);
    type RecordedSessions = HashMap<String, Vec<RecordedEvent>>;

    struct FakeTarget {
        mappings: HashMap<String, ConversationId>,
        records: Mutex<RecordedSessions>,
        append_count: AtomicUsize,
        stall_appends: bool,
        released: AtomicBool,
        release: Notify,
        fail_after: Option<usize>,
    }

    impl FakeTarget {
        fn with_sessions(session_count: usize) -> Self {
            let mappings = (0..session_count)
                .map(|index| {
                    let id = format!("00000000-0000-4000-8000-{:012x}", index + 1);
                    (
                        format!("opaque-{index}"),
                        ConversationId::parse(&id).unwrap(),
                    )
                })
                .collect();
            Self {
                mappings,
                records: Mutex::new(HashMap::new()),
                append_count: AtomicUsize::new(0),
                stall_appends: false,
                released: AtomicBool::new(false),
                release: Notify::new(),
                fail_after: None,
            }
        }

        fn stalled(session_count: usize) -> Self {
            Self {
                stall_appends: true,
                ..Self::with_sessions(session_count)
            }
        }

        fn release(&self) {
            self.released.store(true, Ordering::Release);
            self.release.notify_waiters();
        }
    }

    impl PersistenceTarget for FakeTarget {
        fn resolve(&self, agent_session_id: &str) -> Option<ConversationId> {
            self.mappings.get(agent_session_id).copied()
        }

        fn append<'a>(
            &'a self,
            agent_session_id: &'a str,
            event_type: &'a str,
            payload: Value,
        ) -> AppendFuture<'a> {
            Box::pin(async move {
                while self.stall_appends && !self.released.load(Ordering::Acquire) {
                    self.release.notified().await;
                }
                let count = self.append_count.fetch_add(1, Ordering::AcqRel) + 1;
                if self.fail_after.is_some_and(|limit| count > limit) {
                    return Err(persistence_error(
                        "CONVERSATION_EVENT_APPEND_FAILED",
                        "fake_append",
                        "injected sensitive detail",
                    ));
                }
                let source_seq = payload["sourceSeq"].as_u64().unwrap();
                self.records
                    .lock()
                    .entry(agent_session_id.to_string())
                    .or_default()
                    .push((source_seq, event_type.to_string(), payload));
                Ok(count as u64)
            })
        }

        fn flush(&self) -> FlushFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }

    fn ordered(target: Arc<FakeTarget>) -> OrderedConversationPersistence {
        OrderedConversationPersistence::with_target(target, Duration::from_secs(3))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn global_worker_budget_is_bounded() {
        const SESSION_COUNT: usize = 128;
        const RECORD_COUNT: usize = 10_000;

        let target = Arc::new(FakeTarget::with_sessions(SESSION_COUNT));
        let persistence = ordered(Arc::clone(&target));
        assert_eq!(persistence.active_worker_count(), WRITER_SHARDS);

        for ordinal in 0..RECORD_COUNT {
            let session_index = ordinal % SESSION_COUNT;
            let source_seq = (ordinal / SESSION_COUNT + 1) as u64;
            persistence
                .submit(
                    &format!("opaque-{session_index}"),
                    source_seq,
                    "message_chunk",
                    serde_json::json!({"sourceSeq": source_seq, "ordinal": ordinal}),
                )
                .unwrap();
            let metrics = persistence.metrics();
            assert!(metrics.active_writer_tasks <= WRITER_SHARDS);
            assert!(metrics.pending_records <= GLOBAL_PENDING_RECORDS);
            assert!(metrics.pending_bytes <= GLOBAL_PENDING_BYTES);
            assert!(metrics.max_per_session_pending_records <= PER_SESSION_PENDING_RECORDS);
        }

        persistence.flush_all().await.unwrap();
        let metrics = persistence.metrics();
        assert_eq!(metrics.pending_records, 0);
        assert_eq!(metrics.pending_bytes, 0);
        assert_eq!(metrics.retained_sessions, 0);
        assert!(metrics.max_pending_records <= GLOBAL_PENDING_RECORDS);
        assert!(metrics.max_pending_bytes <= GLOBAL_PENDING_BYTES);
        assert!(metrics.max_per_session_pending_records <= PER_SESSION_PENDING_RECORDS);

        {
            let records = target.records.lock();
            for session_index in 0..SESSION_COUNT {
                let session = format!("opaque-{session_index}");
                let durable = &records[&session];
                assert_eq!(
                    durable.iter().map(|record| record.0).collect::<Vec<_>>(),
                    (1..=durable.len() as u64).collect::<Vec<_>>()
                );
            }
        }
        persistence.shutdown().await.unwrap();
        assert_eq!(persistence.active_worker_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn one_outer_deadline_keeps_runtime_responsive() {
        const SESSION_COUNT: usize = 128;
        let target = Arc::new(FakeTarget::stalled(SESSION_COUNT));
        let persistence = ordered(Arc::clone(&target));
        for session_index in 0..SESSION_COUNT {
            persistence
                .submit(
                    &format!("opaque-{session_index}"),
                    1,
                    "message_chunk",
                    serde_json::json!({"sourceSeq": 1}),
                )
                .unwrap();
        }

        let heartbeat_ticks = Arc::new(AtomicUsize::new(0));
        let heartbeat_counter = Arc::clone(&heartbeat_ticks);
        let heartbeat = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
                heartbeat_counter.fetch_add(1, Ordering::AcqRel);
            }
        });
        let started = Instant::now();
        let deadline = started + DEFAULT_DRAIN_TIMEOUT;
        let error = persistence.shutdown_until(deadline).await.unwrap_err();
        let elapsed = Instant::now().duration_since(started);
        heartbeat.abort();

        assert_eq!(error.code, WRITER_DRAIN_TIMEOUT);
        assert!(elapsed <= Duration::from_secs(31));
        let expected_ticks = (elapsed.as_millis() / 50) as usize;
        assert!(heartbeat_ticks.load(Ordering::Acquire) + 1 >= expected_ticks);
        assert!(heartbeat_ticks.load(Ordering::Acquire) > 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ten_thousand_completed_sessions_are_reaped_after_frontier_observation() {
        const SESSION_COUNT: usize = 10_000;
        let target = Arc::new(FakeTarget::with_sessions(SESSION_COUNT));
        let persistence = ordered(Arc::clone(&target));
        for session_index in 0..SESSION_COUNT {
            persistence
                .submit(
                    &format!("opaque-{session_index}"),
                    1,
                    "prompt_complete",
                    serde_json::json!({"sourceSeq": 1}),
                )
                .unwrap();
        }
        persistence.flush_all().await.unwrap();
        assert_eq!(persistence.retained_worker_count(), 0);
        assert!(persistence.health("opaque-0").unwrap().is_none());

        persistence
            .submit(
                "opaque-0",
                2,
                "prompt_complete",
                serde_json::json!({"sourceSeq": 2}),
            )
            .unwrap();
        persistence.flush_all().await.unwrap();
        assert_eq!(persistence.retained_worker_count(), 0);
        persistence.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unmapped_and_byte_saturated_records_fail_before_admission() {
        let target = Arc::new(FakeTarget::stalled(2));
        let persistence = ordered(Arc::clone(&target));
        let baseline = persistence.metrics();

        let unmapped = persistence
            .submit(
                "unmapped-sensitive-binding",
                1,
                "message_chunk",
                serde_json::json!({"sourceSeq": 1, "prompt": "never expose"}),
            )
            .unwrap_err();
        assert_eq!(unmapped.code, "CONVERSATION_BINDING_NOT_FOUND");
        assert!(!unmapped.to_string().contains("unmapped-sensitive-binding"));
        assert!(!unmapped.to_string().contains("never expose"));
        assert_eq!(persistence.metrics(), baseline);

        let first_blob = "a".repeat(8 * 1024 * 1024);
        persistence
            .submit(
                "opaque-0",
                1,
                "message_chunk",
                serde_json::json!({"sourceSeq": 1, "body": first_blob}),
            )
            .unwrap();
        let after_first = persistence.metrics();
        let second_blob = "b".repeat(9 * 1024 * 1024);
        let saturated = persistence
            .submit(
                "opaque-1",
                1,
                "message_chunk",
                serde_json::json!({"sourceSeq": 1, "body": second_blob, "credential": "never expose"}),
            )
            .unwrap_err();
        assert_eq!(saturated.code, WRITER_BYTES_SATURATED);
        assert!(!saturated.to_string().contains("credential"));
        assert!(!saturated.to_string().contains("never expose"));
        assert_eq!(persistence.metrics(), after_first);

        target.release();
        persistence.flush_all().await.unwrap();
        persistence.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oversized_record_is_rejected_without_allocating_session_state() {
        let target = Arc::new(FakeTarget::with_sessions(1));
        let persistence = ordered(Arc::clone(&target));
        let payload = serde_json::json!({
            "sourceSeq": 1,
            "body": "x".repeat(GLOBAL_PENDING_BYTES + 1),
        });
        let error = persistence
            .submit("opaque-0", 1, "message_chunk", payload)
            .unwrap_err();
        assert_eq!(error.code, WRITER_RECORD_TOO_LARGE);
        let metrics = persistence.metrics();
        assert_eq!(metrics.pending_records, 0);
        assert_eq!(metrics.pending_bytes, 0);
        assert_eq!(metrics.retained_sessions, 0);
        persistence.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn circuit_failure_receipts_remain_secret_safe() {
        let target = Arc::new(FakeTarget {
            fail_after: Some(1),
            ..FakeTarget::with_sessions(1)
        });
        let persistence = ordered(Arc::clone(&target));
        for source_seq in 1..=2 {
            persistence
                .submit(
                    "opaque-0",
                    source_seq,
                    "message_chunk",
                    serde_json::json!({
                        "sourceSeq": source_seq,
                        "prompt": "do-not-log",
                        "terminalIo": "do-not-log",
                        "path": "/sensitive/path"
                    }),
                )
                .unwrap();
        }
        let error = persistence.flush_all().await.unwrap_err();
        assert_eq!(error.code, WRITER_UNHEALTHY);
        for forbidden in ["do-not-log", "opaque-0", "/sensitive/path"] {
            assert!(!error.to_string().contains(forbidden));
        }
        let health = persistence.health("opaque-0").unwrap().unwrap();
        assert_eq!(
            health.last_error_code,
            Some("CONVERSATION_EVENT_APPEND_FAILED")
        );
        assert_eq!(health.last_accepted_source_seq, 2);
        assert_eq!(health.last_persisted_source_seq, 1);
        assert_eq!(health.pending_count, 0);
        assert!(persistence.shutdown().await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn duplicate_and_decreasing_sequences_open_a_retained_circuit() {
        let target = Arc::new(FakeTarget::with_sessions(1));
        let persistence = ordered(Arc::clone(&target));
        persistence
            .submit(
                "opaque-0",
                2,
                "message_chunk",
                serde_json::json!({"sourceSeq": 2}),
            )
            .unwrap();
        for invalid in [2, 1] {
            let error = persistence
                .submit(
                    "opaque-0",
                    invalid,
                    "message_chunk",
                    serde_json::json!({"sourceSeq": invalid, "prompt": "never expose"}),
                )
                .unwrap_err();
            assert_eq!(error.code, SOURCE_SEQUENCE_INVALID);
            assert!(!error.to_string().contains("never expose"));
            assert!(!error.to_string().contains("opaque-0"));
        }
        assert!(persistence.flush_all().await.is_err());
        assert_eq!(target.append_count.load(Ordering::Acquire), 1);
        assert_eq!(persistence.retained_worker_count(), 1);
        assert!(persistence.shutdown().await.is_err());
    }
}
