//! Shared remote-access authentication and authorization authority.
//!
//! The host injects one `Arc<RemoteAccessAuthority>` into the Axum router. Raw
//! credentials are accepted only at provisioning/verification boundaries; the
//! authority retains a SHA-256 digest and compares candidate digests in
//! constant time. Credentials, digests, authorization headers, URL fragments,
//! and recovery provenance must never be logged here.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tracing::{error, info, warn};
use url::Url;

const TOKEN_BYTES: usize = 32;
const MAX_TOKEN_BYTES: usize = 512;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
const FAILURE_LIMIT: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);
const FAILURE_STATE_TTL: Duration = Duration::from_secs(60);

/// Process-wide bound for retained unauthenticated failure state.
pub const MAX_AUTH_FAILURE_STATES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAuthoritySource {
    DesktopKeyring,
    OperatorTokenFile,
    Test,
    Unconfigured,
}

impl RemoteAuthoritySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::DesktopKeyring => "desktop_keyring",
            Self::OperatorTokenFile => "operator_token_file",
            Self::Test => "test",
            Self::Unconfigured => "unconfigured",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoteCapability {
    Connect,
    Read,
    Mutate,
    RecoveryInspect,
}

impl RemoteCapability {
    fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Read => "read",
            Self::Mutate => "mutate",
            Self::RecoveryInspect => "recovery_inspect",
        }
    }
}

/// Identifier-free route metadata attached by `web::router`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoteRouteClass {
    Health,
    AcpWebSocket,
    TerminalWebSocket,
    Project,
    Mcp,
    Filesystem,
    Git,
    Search,
    Skill,
    FrontendLog,
    Workspace,
    Conversation,
    Recovery,
    AcpCatalog,
    AcpInstall,
    Worktree,
}

impl RemoteRouteClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Health => "health",
            Self::AcpWebSocket => "acp_ws",
            Self::TerminalWebSocket => "terminal_ws",
            Self::Project => "project",
            Self::Mcp => "mcp",
            Self::Filesystem => "filesystem",
            Self::Git => "git",
            Self::Search => "search",
            Self::Skill => "skill",
            Self::FrontendLog => "frontend_log",
            Self::Workspace => "workspace",
            Self::Conversation => "conversation",
            Self::Recovery => "recovery",
            Self::AcpCatalog => "acp_catalog",
            Self::AcpInstall => "acp_install",
            Self::Worktree => "worktree",
        }
    }

    fn capability(self, method: &Method) -> Option<RemoteCapability> {
        match self {
            Self::Health => None,
            Self::AcpWebSocket => Some(RemoteCapability::Connect),
            Self::TerminalWebSocket => Some(RemoteCapability::Mutate),
            Self::Recovery => Some(RemoteCapability::RecoveryInspect),
            _ if *method == Method::GET => Some(RemoteCapability::Read),
            _ => Some(RemoteCapability::Mutate),
        }
    }

    fn requires_http_bearer(self) -> bool {
        !matches!(self, Self::Health | Self::AcpWebSocket)
    }

    /// Compatibility fallback for focused routers outside `web::router`.
    /// Production routes attach the enum directly and never log this path.
    fn from_path(path: &str) -> Option<Self> {
        if path == "/health" {
            Some(Self::Health)
        } else if path == "/ws" {
            Some(Self::AcpWebSocket)
        } else if path == "/terminal/ws" {
            Some(Self::TerminalWebSocket)
        } else if path == "/projects" || path.starts_with("/projects/") {
            Some(Self::Project)
        } else if path == "/mcp-servers" || path.starts_with("/mcp-servers/") {
            Some(Self::Mcp)
        } else if path.starts_with("/fs/") || path == "/shells" {
            Some(Self::Filesystem)
        } else if path.starts_with("/git/") {
            Some(Self::Git)
        } else if path.starts_with("/search/") {
            Some(Self::Search)
        } else if path == "/skills" || path.starts_with("/skills/") {
            Some(Self::Skill)
        } else if path.starts_with("/log/") {
            Some(Self::FrontendLog)
        } else if path.starts_with("/workspace/") {
            Some(Self::Workspace)
        } else if path.starts_with("/conversation-recovery/") {
            Some(Self::Recovery)
        } else if path == "/conversations" || path.starts_with("/conversations/") {
            Some(Self::Conversation)
        } else if path == "/acp/catalog" || path.starts_with("/acp/catalog/") {
            Some(Self::AcpCatalog)
        } else if path == "/acp/install" {
            Some(Self::AcpInstall)
        } else if path.starts_with("/worktree/") {
            Some(Self::Worktree)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePrincipal {
    authority_source: RemoteAuthoritySource,
    generation: u64,
}

impl RemotePrincipal {
    #[must_use]
    pub fn authority_source(&self) -> RemoteAuthoritySource {
        self.authority_source
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAuthError {
    Unconfigured,
    InvalidCredential,
    InvalidOrigin,
    RateLimited,
    Forbidden,
    Provisioning,
}

impl RemoteAuthError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unconfigured | Self::Provisioning => "AUTH_CONFIGURATION_ERROR",
            Self::InvalidCredential => "UNAUTHORIZED",
            Self::InvalidOrigin | Self::Forbidden => "FORBIDDEN",
            Self::RateLimited => "RATE_LIMITED",
        }
    }

    #[must_use]
    pub const fn status(self) -> StatusCode {
        match self {
            Self::InvalidCredential => StatusCode::UNAUTHORIZED,
            Self::InvalidOrigin | Self::Forbidden => StatusCode::FORBIDDEN,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Unconfigured | Self::Provisioning => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn safe_message(self) -> &'static str {
        match self {
            Self::Unconfigured | Self::Provisioning => {
                "remote access authentication is not configured"
            }
            Self::InvalidCredential => "remote access credential is missing or invalid",
            Self::InvalidOrigin => "request Origin is missing or not allowed",
            Self::RateLimited => "too many failed authentication attempts",
            Self::Forbidden => "remote principal lacks the required capability",
        }
    }
}

impl std::fmt::Display for RemoteAuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.safe_message())
    }
}

impl std::error::Error for RemoteAuthError {}

/// One desktop credential generation. The host owns this lease and the raw
/// bearer; the authority retains only the generation metadata and digest.
pub struct DesktopCredentialLease {
    generation: u64,
    bearer: String,
}

impl DesktopCredentialLease {
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub(crate) fn bearer(&self) -> &str {
        &self.bearer
    }
}

struct CredentialState {
    generation: u64,
    digest: Option<[u8; 32]>,
    source: RemoteAuthoritySource,
    desktop_keyring_account: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FailureKey {
    peer: IpAddr,
    generation: u64,
}

#[derive(Debug, Clone)]
struct FailureState {
    window_started: Instant,
    failures: u32,
    locked_until: Option<Instant>,
    last_touched: Instant,
    lru_order: u64,
}

#[derive(Default)]
struct FailureLimiter {
    states: HashMap<FailureKey, FailureState>,
    next_lru_order: u64,
}

impl FailureLimiter {
    fn prune_expired(&mut self, now: Instant) {
        self.states.retain(|_, state| {
            now.saturating_duration_since(state.last_touched) < FAILURE_STATE_TTL
        });
    }

    fn next_lru_order(&mut self) -> u64 {
        self.next_lru_order = self.next_lru_order.saturating_add(1);
        self.next_lru_order
    }

    fn evict_lru_if_full(&mut self) {
        if self.states.len() < MAX_AUTH_FAILURE_STATES {
            return;
        }
        if let Some(oldest) = self
            .states
            .iter()
            .min_by_key(|(_, state)| state.lru_order)
            .map(|(key, _)| *key)
        {
            self.states.remove(&oldest);
        }
    }

    fn record_failure(&mut self, key: FailureKey, now: Instant) -> RemoteAuthError {
        self.prune_expired(now);
        if !self.states.contains_key(&key) {
            self.evict_lru_if_full();
        }
        let lru_order = self.next_lru_order();
        let state = self.states.entry(key).or_insert(FailureState {
            window_started: now,
            failures: 0,
            locked_until: None,
            last_touched: now,
            lru_order,
        });
        state.last_touched = now;
        state.lru_order = lru_order;
        if now.saturating_duration_since(state.window_started) >= FAILURE_WINDOW {
            state.window_started = now;
            state.failures = 0;
            state.locked_until = None;
        }
        if state.locked_until.is_some_and(|until| until > now) {
            return RemoteAuthError::RateLimited;
        }
        state.failures = state.failures.saturating_add(1);
        if state.failures > FAILURE_LIMIT {
            state.locked_until = Some(now + LOCKOUT);
            RemoteAuthError::RateLimited
        } else {
            RemoteAuthError::InvalidCredential
        }
    }

    fn len_at(&mut self, now: Instant) -> usize {
        self.prune_expired(now);
        self.states.len()
    }
}

/// Host-owned remote-access credential and policy authority.
pub struct RemoteAccessAuthority {
    credential: RwLock<CredentialState>,
    allowed_origins: RwLock<HashSet<String>>,
    failures: Mutex<FailureLimiter>,
}

impl std::fmt::Debug for RemoteAccessAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let credential = self.credential.read();
        formatter
            .debug_struct("RemoteAccessAuthority")
            .field("source", &credential.source)
            .field("generation", &credential.generation)
            .field("configured", &credential.digest.is_some())
            .field("allowed_origin_count", &self.allowed_origins.read().len())
            .finish_non_exhaustive()
    }
}

impl RemoteAccessAuthority {
    #[must_use]
    pub fn unconfigured() -> Self {
        Self {
            credential: RwLock::new(CredentialState {
                generation: 0,
                digest: None,
                source: RemoteAuthoritySource::Unconfigured,
                desktop_keyring_account: None,
            }),
            allowed_origins: RwLock::new(HashSet::new()),
            failures: Mutex::new(FailureLimiter::default()),
        }
    }

    /// Load the desktop credential from the OS keyring, or issue and persist a
    /// 256-bit credential when the entry does not exist. The returned token is
    /// for the immediate pairing URL only; the authority stores its digest.
    pub fn issue_or_load_desktop(keyring_account: &str) -> Result<(Self, String), RemoteAuthError> {
        if keyring_account.trim().is_empty() {
            return Err(RemoteAuthError::Provisioning);
        }
        let (token, issued) = match crate::secure_storage::keyring_get(keyring_account) {
            Ok(Some(token)) => (token, false),
            Ok(None) => {
                let token = generate_token().inspect_err(|error| {
                    error!(
                        target: "termul::web::auth",
                        authority_source = RemoteAuthoritySource::DesktopKeyring.as_str(),
                        operation = "generate_credential",
                        stable_code = error.code(),
                        "remote access authority provisioning failed"
                    );
                })?;
                crate::secure_storage::keyring_set(keyring_account, &token).map_err(|_| {
                    error!(
                        target: "termul::web::auth",
                        authority_source = RemoteAuthoritySource::DesktopKeyring.as_str(),
                        operation = "keyring_set",
                        stable_code = RemoteAuthError::Provisioning.code(),
                        "remote access authority provisioning failed"
                    );
                    RemoteAuthError::Provisioning
                })?;
                (token, true)
            }
            Err(_) => {
                error!(
                    target: "termul::web::auth",
                    authority_source = RemoteAuthoritySource::DesktopKeyring.as_str(),
                    operation = "keyring_get",
                    stable_code = RemoteAuthError::Provisioning.code(),
                    "remote access authority provisioning failed"
                );
                return Err(RemoteAuthError::Provisioning);
            }
        };
        validate_token(&token).inspect_err(|error| {
            error!(
                target: "termul::web::auth",
                authority_source = RemoteAuthoritySource::DesktopKeyring.as_str(),
                operation = "validate_credential",
                stable_code = error.code(),
                "remote access authority provisioning failed"
            );
        })?;
        let authority = Self::from_token(
            &token,
            RemoteAuthoritySource::DesktopKeyring,
            Some(keyring_account.to_string()),
        );
        info!(
            target: "termul::web::auth",
            authority_source = RemoteAuthoritySource::DesktopKeyring.as_str(),
            generation = 1_u64,
            provisioned = issued,
            "remote access authority ready"
        );
        Ok((authority, token))
    }

    /// Load a standalone credential from an explicit operator-owned file.
    /// On Unix, group/world permission bits are rejected before reading.
    pub fn from_token_file(path: &Path) -> Result<Self, RemoteAuthError> {
        validate_token_file_permissions(path).inspect_err(|error| {
            error!(
                target: "termul::web::auth",
                authority_source = RemoteAuthoritySource::OperatorTokenFile.as_str(),
                operation = "validate_token_file",
                stable_code = error.code(),
                "remote access authority provisioning failed"
            );
        })?;
        let token = std::fs::read_to_string(path).map_err(|_| {
            error!(
                target: "termul::web::auth",
                authority_source = RemoteAuthoritySource::OperatorTokenFile.as_str(),
                operation = "read_token_file",
                stable_code = RemoteAuthError::Provisioning.code(),
                "remote access authority provisioning failed"
            );
            RemoteAuthError::Provisioning
        })?;
        let token = token.trim_end_matches(['\r', '\n']);
        validate_token(token).inspect_err(|error| {
            error!(
                target: "termul::web::auth",
                authority_source = RemoteAuthoritySource::OperatorTokenFile.as_str(),
                operation = "validate_credential",
                stable_code = error.code(),
                "remote access authority provisioning failed"
            );
        })?;
        let authority = Self::from_token(token, RemoteAuthoritySource::OperatorTokenFile, None);
        info!(
            target: "termul::web::auth",
            authority_source = RemoteAuthoritySource::OperatorTokenFile.as_str(),
            "remote access authority ready"
        );
        Ok(authority)
    }

    fn from_token(
        token: &str,
        source: RemoteAuthoritySource,
        desktop_keyring_account: Option<String>,
    ) -> Self {
        Self {
            credential: RwLock::new(CredentialState {
                generation: 1,
                digest: Some(digest(token.as_bytes())),
                source,
                desktop_keyring_account,
            }),
            allowed_origins: RwLock::new(HashSet::new()),
            failures: Mutex::new(FailureLimiter::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(token: &str) -> Self {
        Self::from_token(token, RemoteAuthoritySource::Test, None)
    }

    /// Generate and install a fresh desktop bearer generation. The raw bearer
    /// is returned exactly once in the host-owned lease; only its digest and
    /// monotonically increasing generation remain in the authority.
    pub fn rotate_desktop_credential(&self) -> Result<DesktopCredentialLease, RemoteAuthError> {
        let bearer = generate_token().inspect_err(|error| {
            error!(
                target: "termul::web::auth",
                operation = "rotate_credential",
                stable_code = error.code(),
                "remote access credential rotation failed"
            );
        })?;
        let mut credential = self.credential.write();
        let generation = credential
            .generation
            .checked_add(1)
            .ok_or(RemoteAuthError::Provisioning)?;
        match credential.source {
            RemoteAuthoritySource::DesktopKeyring => {
                let account = credential
                    .desktop_keyring_account
                    .as_deref()
                    .ok_or(RemoteAuthError::Provisioning)?;
                crate::secure_storage::keyring_set(account, &bearer).map_err(|_| {
                    error!(
                        target: "termul::web::auth",
                        authority_source = RemoteAuthoritySource::DesktopKeyring.as_str(),
                        operation = "rotate_keyring_set",
                        stable_code = RemoteAuthError::Provisioning.code(),
                        "remote access credential rotation failed"
                    );
                    RemoteAuthError::Provisioning
                })?;
            }
            RemoteAuthoritySource::Test => {}
            RemoteAuthoritySource::OperatorTokenFile | RemoteAuthoritySource::Unconfigured => {
                return Err(RemoteAuthError::Provisioning);
            }
        }
        credential.generation = generation;
        credential.digest = Some(digest(bearer.as_bytes()));
        let source = credential.source;
        drop(credential);
        self.allowed_origins.write().clear();
        *self.failures.lock() = FailureLimiter::default();
        info!(
            target: "termul::web::auth",
            authority_source = source.as_str(),
            generation,
            lifecycle_phase = "rotate",
            stable_code = "OK",
            "remote access credential generation rotated"
        );
        Ok(DesktopCredentialLease { generation, bearer })
    }

    /// Invalidate only the named generation. A stale compensation path cannot
    /// clear a newer generation that won a lifecycle race.
    pub fn invalidate_generation(&self, generation: u64) {
        let invalidated = {
            let mut credential = self.credential.write();
            if credential.generation == generation && credential.digest.is_some() {
                credential.digest = None;
                true
            } else {
                false
            }
        };
        if invalidated {
            self.allowed_origins.write().clear();
            *self.failures.lock() = FailureLimiter::default();
            warn!(
                target: "termul::web::auth",
                generation,
                lifecycle_phase = "invalidate",
                stable_code = "GENERATION_INVALIDATED",
                "remote access credential generation invalidated"
            );
        }
    }

    /// Replace the credential digest without retaining the raw credential.
    pub fn install_credential(
        &self,
        token: &str,
        source: RemoteAuthoritySource,
    ) -> Result<(), RemoteAuthError> {
        validate_token(token)?;
        let mut credential = self.credential.write();
        credential.generation = credential
            .generation
            .checked_add(1)
            .ok_or(RemoteAuthError::Provisioning)?;
        credential.digest = Some(digest(token.as_bytes()));
        credential.source = source;
        if source != RemoteAuthoritySource::DesktopKeyring {
            credential.desktop_keyring_account = None;
        }
        drop(credential);
        *self.failures.lock() = FailureLimiter::default();
        Ok(())
    }

    pub fn set_public_origin(&self, origin: Url) -> Result<(), RemoteAuthError> {
        let normalized = normalize_origin(&origin)?;
        self.allowed_origins.write().insert(normalized);
        let generation = self.credential.read().generation;
        info!(
            target: "termul::web::auth",
            generation,
            lifecycle_phase = "register_origin",
            stable_code = "OK",
            "remote access Origin policy updated"
        );
        Ok(())
    }

    pub fn set_allowed_origins<I>(&self, origins: I) -> Result<(), RemoteAuthError>
    where
        I: IntoIterator<Item = Url>,
    {
        let normalized = origins
            .into_iter()
            .map(|origin| normalize_origin(&origin))
            .collect::<Result<HashSet<_>, _>>()?;
        if normalized.is_empty() {
            return Err(RemoteAuthError::InvalidOrigin);
        }
        *self.allowed_origins.write() = normalized;
        Ok(())
    }

    pub fn verify_bearer(&self, token: &str) -> Result<RemotePrincipal, RemoteAuthError> {
        validate_token(token).map_err(|_| RemoteAuthError::InvalidCredential)?;
        let credential = self.credential.read();
        let expected = match credential.digest {
            Some(expected) => expected,
            None if credential.generation > 0
                && credential.source != RemoteAuthoritySource::Unconfigured =>
            {
                return Err(RemoteAuthError::InvalidCredential);
            }
            None => return Err(RemoteAuthError::Unconfigured),
        };
        let candidate = digest(token.as_bytes());
        if bool::from(expected.ct_eq(&candidate)) {
            Ok(RemotePrincipal {
                authority_source: credential.source,
                generation: credential.generation,
            })
        } else {
            Err(RemoteAuthError::InvalidCredential)
        }
    }

    pub fn verify_ws_auth(
        &self,
        token: &str,
        origin: Option<&HeaderValue>,
    ) -> Result<RemotePrincipal, RemoteAuthError> {
        self.verify_origin(origin)?;
        self.verify_bearer(token)
    }

    pub fn authorize(
        &self,
        principal: &RemotePrincipal,
        capability: RemoteCapability,
    ) -> Result<(), RemoteAuthError> {
        let credential = self.credential.read();
        if credential.digest.is_none()
            || principal.generation != credential.generation
            || principal.authority_source != credential.source
            || principal.authority_source == RemoteAuthoritySource::Unconfigured
        {
            warn!(
                target: "termul::web::auth",
                generation = principal.generation,
                capability = capability.as_str(),
                stable_code = RemoteAuthError::Forbidden.code(),
                "remote capability rejected"
            );
            return Err(RemoteAuthError::Forbidden);
        }
        Ok(())
    }

    pub fn verify_origin(&self, origin: Option<&HeaderValue>) -> Result<(), RemoteAuthError> {
        let raw = origin
            .and_then(|value| value.to_str().ok())
            .ok_or(RemoteAuthError::InvalidOrigin)?;
        let parsed = Url::parse(raw).map_err(|_| RemoteAuthError::InvalidOrigin)?;
        let normalized = normalize_origin(&parsed)?;
        if self.allowed_origins.read().contains(&normalized) {
            Ok(())
        } else {
            Err(RemoteAuthError::InvalidOrigin)
        }
    }

    pub fn verify_bearer_for_peer(
        &self,
        token: &str,
        peer: IpAddr,
    ) -> Result<RemotePrincipal, RemoteAuthError> {
        self.verify_bearer_for_peer_at(token, peer, Instant::now())
    }

    fn verify_bearer_for_peer_at(
        &self,
        token: &str,
        peer: IpAddr,
        now: Instant,
    ) -> Result<RemotePrincipal, RemoteAuthError> {
        // Always verify the current credential before consulting failure state.
        // A forged burst through one shared loopback proxy therefore cannot
        // lock out a caller that presents the correct generation bearer.
        match self.verify_bearer(token) {
            Ok(principal) => Ok(principal),
            Err(error) => {
                let generation = {
                    let credential = self.credential.read();
                    if credential.generation == 0
                        || credential.source == RemoteAuthoritySource::Unconfigured
                    {
                        return Err(error);
                    }
                    credential.generation
                };
                let reported = self
                    .failures
                    .lock()
                    .record_failure(FailureKey { peer, generation }, now);
                warn!(
                    target: "termul::web::auth",
                    generation,
                    auth_class = "bearer",
                    stable_code = reported.code(),
                    "remote authentication failed"
                );
                Err(reported)
            }
        }
    }

    /// Number of retained unauthenticated failure states after TTL cleanup.
    #[must_use]
    pub fn failure_state_count(&self) -> usize {
        self.failures.lock().len_at(Instant::now())
    }

    #[cfg(test)]
    fn failure_state_count_at(&self, now: Instant) -> usize {
        self.failures.lock().len_at(now)
    }
}

fn generate_token() -> Result<String, RemoteAuthError> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|_| RemoteAuthError::Provisioning)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn validate_token(token: &str) -> Result<(), RemoteAuthError> {
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES || token.trim() != token {
        return Err(RemoteAuthError::InvalidCredential);
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn normalize_origin(origin: &Url) -> Result<String, RemoteAuthError> {
    if !matches!(origin.scheme(), "http" | "https")
        || origin.host_str().is_none()
        || origin.username() != ""
        || origin.password().is_some()
    {
        return Err(RemoteAuthError::InvalidOrigin);
    }
    let mut normalized = format!("{}://{}", origin.scheme(), origin.host_str().unwrap());
    if let Some(port) = origin.port() {
        normalized.push(':');
        normalized.push_str(&port.to_string());
    }
    Ok(normalized)
}

#[cfg(unix)]
fn validate_token_file_permissions(path: &Path) -> Result<(), RemoteAuthError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| RemoteAuthError::Provisioning)?;
    // SAFETY: `geteuid` has no preconditions and only reads the process's
    // effective user id. Reject files owned by any other account.
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_file()
        || metadata.uid() != effective_uid
        || metadata.mode() & 0o077 != 0
    {
        return Err(RemoteAuthError::Provisioning);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_token_file_permissions(path: &Path) -> Result<(), RemoteAuthError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| RemoteAuthError::Provisioning)?;
    if !metadata.file_type().is_file() {
        return Err(RemoteAuthError::Provisioning);
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthFailureBody {
    success: bool,
    error: &'static str,
    code: &'static str,
}

pub fn auth_error_response(error: RemoteAuthError) -> Response {
    (
        error.status(),
        Json(AuthFailureBody {
            success: false,
            error: error.safe_message(),
            code: error.code(),
        }),
    )
        .into_response()
}

pub async fn capability_middleware(
    Extension(authority): Extension<Arc<RemoteAccessAuthority>>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let route_class = request
        .extensions()
        .get::<RemoteRouteClass>()
        .copied()
        .or_else(|| RemoteRouteClass::from_path(request.uri().path()));
    let Some(route_class) = route_class else {
        return next.run(request).await;
    };
    let method = request.method().clone();
    let Some(capability) = route_class.capability(&method) else {
        return next.run(request).await;
    };
    let started = Instant::now();

    // ACP WebSocket bearer authentication occurs in its first protocol frame;
    // this HTTP boundary records only identifier-free route metadata.
    if !route_class.requires_http_bearer() {
        let response = next.run(request).await;
        let stable_code = if response.status().is_success()
            || response.status() == StatusCode::SWITCHING_PROTOCOLS
        {
            "OK"
        } else {
            "APPLICATION_ERROR"
        };
        log_boundary_outcome(
            &method,
            route_class,
            capability,
            stable_code,
            response.status(),
            started.elapsed(),
        );
        return response;
    }

    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::from([0, 0, 0, 0]), |value| value.0.ip());
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(token) = token else {
        let error = authority
            .verify_bearer_for_peer("", peer)
            .err()
            .unwrap_or(RemoteAuthError::InvalidCredential);
        log_boundary_outcome(
            &method,
            route_class,
            capability,
            error.code(),
            error.status(),
            started.elapsed(),
        );
        return auth_error_response(error);
    };
    let principal = match authority.verify_bearer_for_peer(token, peer) {
        Ok(principal) => principal,
        Err(error) => {
            log_boundary_outcome(
                &method,
                route_class,
                capability,
                error.code(),
                error.status(),
                started.elapsed(),
            );
            return auth_error_response(error);
        }
    };
    if let Err(error) = authority.authorize(&principal, capability) {
        log_boundary_outcome(
            &method,
            route_class,
            capability,
            error.code(),
            error.status(),
            started.elapsed(),
        );
        return auth_error_response(error);
    }
    request.extensions_mut().insert(principal);
    let response = next.run(request).await;
    let stable_code = if response.status().is_success() {
        "OK"
    } else {
        "APPLICATION_ERROR"
    };
    log_boundary_outcome(
        &method,
        route_class,
        capability,
        stable_code,
        response.status(),
        started.elapsed(),
    );
    response
}

fn log_boundary_outcome(
    method: &Method,
    route_class: RemoteRouteClass,
    capability: RemoteCapability,
    stable_code: &str,
    status: StatusCode,
    duration: Duration,
) {
    if status.is_client_error() || status.is_server_error() {
        warn!(
            target: "termul::web::auth",
            method = method.as_str(),
            route_class = route_class.as_str(),
            capability = capability.as_str(),
            stable_code,
            http_status = status.as_u16(),
            duration_ms = duration.as_millis(),
            "remote boundary request rejected"
        );
    } else {
        info!(
            target: "termul::web::auth",
            method = method.as_str(),
            route_class = route_class.as_str(),
            capability = capability.as_str(),
            stable_code,
            http_status = status.as_u16(),
            duration_ms = duration.as_millis(),
            "remote boundary request completed"
        );
    }
}

/// Stable application-code to HTTP-status mapping shared by Conversation
/// adapters. Bodies retain their existing camelCase `IpcBody<T>` envelope.
#[must_use]
pub fn status_for_code(code: &str) -> StatusCode {
    match code {
        "VALIDATION_ERROR" | "CONVERSATION_INVALID_ID" => StatusCode::BAD_REQUEST,
        "UNAUTHORIZED" => StatusCode::UNAUTHORIZED,
        "FORBIDDEN" => StatusCode::FORBIDDEN,
        "CONVERSATION_NOT_FOUND" | "RECOVERY_NOT_FOUND" => StatusCode::NOT_FOUND,
        "CONVERSATION_CONFLICT"
        | "CONVERSATION_LIVE_RESOURCES"
        | "LEGACY_ID_AMBIGUOUS"
        | "MIGRATION_IDEMPOTENCY_CONFLICT" => StatusCode::CONFLICT,
        "CONVERSATION_RECOVERY_REQUIRED"
        | "ACP_COMPENSATION_FAILED"
        | "LEGACY_COMPATIBILITY_READ_ONLY" => StatusCode::UNPROCESSABLE_ENTITY,
        "CONVERSATION_SERVICE_UNAVAILABLE" | "SESSION_WORKSPACE_UNAVAILABLE" => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use std::io::Write;
    use std::net::Ipv6Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use tower::ServiceExt;

    const TOKEN: &str = "test-remote-access-token";

    #[derive(Clone, Default)]
    struct LogBuffer(Arc<StdMutex<Vec<u8>>>);

    struct LogWriter(Arc<StdMutex<Vec<u8>>>);

    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for LogBuffer {
        type Writer = LogWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            LogWriter(Arc::clone(&self.0))
        }
    }

    impl LogBuffer {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn authority() -> RemoteAccessAuthority {
        let authority = RemoteAccessAuthority::for_tests(TOKEN);
        authority
            .set_public_origin(Url::parse("https://example.test/path").unwrap())
            .unwrap();
        authority
    }

    #[test]
    fn credential_verification_rejects_empty_wrong_and_oversized() {
        let authority = authority();
        assert!(authority.verify_bearer(TOKEN).is_ok());
        assert_eq!(
            authority.verify_bearer("").unwrap_err(),
            RemoteAuthError::InvalidCredential
        );
        assert_eq!(
            authority.verify_bearer("wrong").unwrap_err(),
            RemoteAuthError::InvalidCredential
        );
        assert_eq!(
            authority
                .verify_bearer(&"x".repeat(MAX_TOKEN_BYTES + 1))
                .unwrap_err(),
            RemoteAuthError::InvalidCredential
        );
    }

    #[test]
    fn digest_only_constant_time_comparison_is_pinned() {
        let source = include_str!("auth.rs");
        assert!(source.contains("Sha256::digest"));
        assert!(source.contains("ct_eq"));
        assert!(!source.contains(&["credential", ": String"].concat()));
        assert!(!source.contains(&["token", " =="].concat()));
    }

    #[test]
    fn websocket_origin_is_required_and_normalized() {
        let authority = authority();
        let allowed = HeaderValue::from_static("https://example.test");
        let wrong = HeaderValue::from_static("https://evil.test");
        assert!(authority.verify_ws_auth(TOKEN, Some(&allowed)).is_ok());
        assert_eq!(
            authority.verify_ws_auth(TOKEN, None).unwrap_err(),
            RemoteAuthError::InvalidOrigin
        );
        assert_eq!(
            authority.verify_ws_auth(TOKEN, Some(&wrong)).unwrap_err(),
            RemoteAuthError::InvalidOrigin
        );
    }

    #[test]
    fn forged_proxy_failures_rate_limit_invalid_tokens_but_never_the_valid_bearer() {
        let authority = authority();
        let peer = IpAddr::from([127, 0, 0, 1]);
        for _ in 0..FAILURE_LIMIT {
            assert_eq!(
                authority.verify_bearer_for_peer("wrong", peer).unwrap_err(),
                RemoteAuthError::InvalidCredential
            );
        }
        assert_eq!(
            authority.verify_bearer_for_peer("wrong", peer).unwrap_err(),
            RemoteAuthError::RateLimited
        );
        assert!(authority.verify_bearer_for_peer(TOKEN, peer).is_ok());
        assert_eq!(
            authority.verify_bearer_for_peer("wrong", peer).unwrap_err(),
            RemoteAuthError::RateLimited
        );
    }

    #[test]
    fn desktop_generation_rotation_invalidates_stale_bearers() {
        let authority = authority();
        let first = authority.rotate_desktop_credential().unwrap();
        let first_bearer = first.bearer().to_string();
        assert!(authority.verify_bearer(&first_bearer).is_ok());
        authority.invalidate_generation(first.generation());
        assert_eq!(
            authority.verify_bearer(&first_bearer).unwrap_err(),
            RemoteAuthError::InvalidCredential
        );

        let second = authority.rotate_desktop_credential().unwrap();
        assert!(second.generation() > first.generation());
        assert_ne!(second.bearer(), first_bearer);
        assert_eq!(
            authority.verify_bearer(&first_bearer).unwrap_err(),
            RemoteAuthError::InvalidCredential
        );
        assert!(authority.verify_bearer(second.bearer()).is_ok());
    }

    #[test]
    fn failure_state_is_ttl_lru_bounded_under_high_cardinality_attack() {
        let authority = authority();
        let now = Instant::now();
        for index in 0_u128..10_000 {
            let peer = IpAddr::V6(Ipv6Addr::from(index + 1));
            assert_eq!(
                authority
                    .verify_bearer_for_peer_at("wrong", peer, now)
                    .unwrap_err(),
                RemoteAuthError::InvalidCredential
            );
        }
        assert!(authority.failure_state_count_at(now) <= MAX_AUTH_FAILURE_STATES);
        assert_eq!(
            authority.failure_state_count_at(now + FAILURE_STATE_TTL + Duration::from_secs(1)),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn token_file_rejects_group_or_world_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, TOKEN).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            RemoteAccessAuthority::from_token_file(&path).unwrap_err(),
            RemoteAuthError::Provisioning
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(RemoteAccessAuthority::from_token_file(&path).is_ok());

        let link = dir.path().join("token-link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(
            RemoteAccessAuthority::from_token_file(&link).unwrap_err(),
            RemoteAuthError::Provisioning
        );
    }

    fn protected_test_router(authority: Arc<RemoteAccessAuthority>) -> axum::Router {
        axum::Router::new()
            .route(
                "/conversations",
                get(|| async { "sensitive-workspace-path" }),
            )
            .layer(axum::middleware::from_fn(capability_middleware))
            .layer(Extension(RemoteRouteClass::Conversation))
            .layer(Extension(authority))
    }

    fn protected_request(authorization: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .uri("/conversations")
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 3000))));
        if let Some(value) = authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn protected_http_rejects_missing_wrong_and_oversized_credentials_without_body_leak() {
        let oversized = format!("Bearer {}", "x".repeat(MAX_TOKEN_BYTES + 1));
        for authorization in [None, Some("Bearer wrong"), Some(oversized.as_str())] {
            let app = protected_test_router(Arc::new(authority()));
            let response = app.oneshot(protected_request(authorization)).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let text = String::from_utf8_lossy(&body);
            assert!(text.contains("UNAUTHORIZED"));
            assert!(!text.contains("sensitive-workspace-path"));
        }
    }

    #[tokio::test]
    async fn loopback_proxy_without_credential_cannot_reach_lifecycle_or_workspace_mutations() {
        let reached = Arc::new(AtomicUsize::new(0));
        let handler_reached = Arc::clone(&reached);
        let authority = Arc::new(authority());
        let app = axum::Router::new()
            .route(
                "/conversations/{conversationId}/lifecycle/detach",
                post(move || {
                    let handler_reached = Arc::clone(&handler_reached);
                    async move {
                        handler_reached.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NO_CONTENT
                    }
                }),
            )
            .route(
                "/conversations/{conversationId}/workspace",
                post({
                    let reached = Arc::clone(&reached);
                    move || {
                        let reached = Arc::clone(&reached);
                        async move {
                            reached.fetch_add(1, Ordering::SeqCst);
                            StatusCode::NO_CONTENT
                        }
                    }
                }),
            )
            .layer(axum::middleware::from_fn(capability_middleware))
            .layer(Extension(RemoteRouteClass::Conversation))
            .layer(Extension(authority));

        for uri in [
            "/conversations/c-1/lifecycle/detach",
            "/conversations/c-1/workspace",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(uri)
                        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 43123))))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
        assert_eq!(reached.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn protected_http_accepts_bearer_and_rate_limits_sixth_failure() {
        let authority = Arc::new(authority());
        let app = protected_test_router(Arc::clone(&authority));
        let accepted = app
            .clone()
            .oneshot(protected_request(Some(&format!("Bearer {TOKEN}"))))
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);

        for attempt in 1..=FAILURE_LIMIT + 1 {
            let response = app
                .clone()
                .oneshot(protected_request(Some("Bearer wrong")))
                .await
                .unwrap();
            let expected = if attempt > FAILURE_LIMIT {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::UNAUTHORIZED
            };
            assert_eq!(response.status(), expected, "attempt {attempt}");
        }
    }

    #[tokio::test]
    async fn captured_boundary_log_uses_static_class_without_path_identifier_or_credential() {
        let logs = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(logs.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let authority = Arc::new(authority());
        let app = axum::Router::new()
            .route(
                "/conversations/{conversationId}/lifecycle/detach",
                post(|| async { StatusCode::NO_CONTENT }),
            )
            .layer(axum::middleware::from_fn(capability_middleware))
            .layer(Extension(RemoteRouteClass::Conversation))
            .layer(Extension(authority));
        let supplied_path = "/conversations/supplied-conversation-id/lifecycle/detach";
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(supplied_path)
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 43123))))
                    .body(Body::from("supplied-sensitive-payload"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let output = logs.text();
        assert!(output.contains("route_class"));
        assert!(output.contains("conversation"));
        assert!(output.contains("capability"));
        assert!(!output.contains(supplied_path));
        assert!(!output.contains("supplied-conversation-id"));
        assert!(!output.contains(TOKEN));
        assert!(!output.contains("supplied-sensitive-payload"));
    }

    #[test]
    fn route_classes_and_boundary_logging_are_identifier_free() {
        for (path, expected) in [
            (
                "/conversations/conversation-secret/lifecycle/detach",
                RemoteRouteClass::Conversation,
            ),
            ("/projects/default", RemoteRouteClass::Project),
            ("/worktree/remove", RemoteRouteClass::Worktree),
            ("/conversation-recovery/resolve", RemoteRouteClass::Recovery),
            ("/ws", RemoteRouteClass::AcpWebSocket),
            ("/terminal/ws", RemoteRouteClass::TerminalWebSocket),
        ] {
            assert_eq!(RemoteRouteClass::from_path(path), Some(expected));
            assert!(!expected.as_str().contains("secret"));
            assert!(!expected.as_str().contains('/'));
        }
        let source = include_str!("auth.rs");
        assert!(!source.contains(&["request_type", " = format!"].concat()));
        assert!(!source.contains(&["request_type", ","].concat()));
    }

    #[test]
    fn application_status_mapping_is_stable() {
        for (code, expected) in [
            ("VALIDATION_ERROR", StatusCode::BAD_REQUEST),
            ("UNAUTHORIZED", StatusCode::UNAUTHORIZED),
            ("FORBIDDEN", StatusCode::FORBIDDEN),
            ("CONVERSATION_NOT_FOUND", StatusCode::NOT_FOUND),
            ("CONVERSATION_CONFLICT", StatusCode::CONFLICT),
            (
                "CONVERSATION_RECOVERY_REQUIRED",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                "CONVERSATION_DURABILITY_FAILED",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ] {
            assert_eq!(status_for_code(code), expected, "{code}");
        }
    }
}
