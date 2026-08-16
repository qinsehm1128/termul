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
    Read,
    Mutate,
    RecoveryInspect,
}

impl RemoteCapability {
    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Mutate => "mutate",
            Self::RecoveryInspect => "recovery_inspect",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePrincipal {
    authority_source: RemoteAuthoritySource,
}

impl RemotePrincipal {
    #[must_use]
    pub fn authority_source(&self) -> RemoteAuthoritySource {
        self.authority_source
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

#[derive(Debug, Clone)]
struct FailureState {
    window_started: Instant,
    failures: u32,
    locked_until: Option<Instant>,
}

/// Host-owned remote-access credential and policy authority.
pub struct RemoteAccessAuthority {
    credential_digest: RwLock<Option<[u8; 32]>>,
    allowed_origins: RwLock<HashSet<String>>,
    failures: Mutex<HashMap<IpAddr, FailureState>>,
    source: RwLock<RemoteAuthoritySource>,
}

impl std::fmt::Debug for RemoteAccessAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteAccessAuthority")
            .field("source", &*self.source.read())
            .field("configured", &self.credential_digest.read().is_some())
            .field("allowed_origin_count", &self.allowed_origins.read().len())
            .finish_non_exhaustive()
    }
}

impl RemoteAccessAuthority {
    #[must_use]
    pub fn unconfigured() -> Self {
        Self {
            credential_digest: RwLock::new(None),
            allowed_origins: RwLock::new(HashSet::new()),
            failures: Mutex::new(HashMap::new()),
            source: RwLock::new(RemoteAuthoritySource::Unconfigured),
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
        let authority = Self::from_token(&token, RemoteAuthoritySource::DesktopKeyring);
        info!(
            target: "termul::web::auth",
            authority_source = RemoteAuthoritySource::DesktopKeyring.as_str(),
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
        let authority = Self::from_token(token, RemoteAuthoritySource::OperatorTokenFile);
        info!(
            target: "termul::web::auth",
            authority_source = RemoteAuthoritySource::OperatorTokenFile.as_str(),
            "remote access authority ready"
        );
        Ok(authority)
    }

    fn from_token(token: &str, source: RemoteAuthoritySource) -> Self {
        Self {
            credential_digest: RwLock::new(Some(digest(token.as_bytes()))),
            allowed_origins: RwLock::new(HashSet::new()),
            failures: Mutex::new(HashMap::new()),
            source: RwLock::new(source),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(token: &str) -> Self {
        Self::from_token(token, RemoteAuthoritySource::Test)
    }

    /// Replace the credential digest without retaining the raw credential.
    pub fn install_credential(
        &self,
        token: &str,
        source: RemoteAuthoritySource,
    ) -> Result<(), RemoteAuthError> {
        validate_token(token)?;
        *self.credential_digest.write() = Some(digest(token.as_bytes()));
        *self.source.write() = source;
        self.failures.lock().clear();
        Ok(())
    }

    pub fn set_public_origin(&self, origin: Url) -> Result<(), RemoteAuthError> {
        let normalized = normalize_origin(&origin)?;
        self.allowed_origins.write().insert(normalized.clone());
        info!(
            target: "termul::web::auth",
            normalized_origin = %normalized,
            "remote access Origin registered"
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
        let expected = self
            .credential_digest
            .read()
            .as_ref()
            .copied()
            .ok_or(RemoteAuthError::Unconfigured)?;
        let candidate = digest(token.as_bytes());
        if bool::from(expected.ct_eq(&candidate)) {
            Ok(RemotePrincipal {
                authority_source: *self.source.read(),
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
        if principal.authority_source != *self.source.read()
            || principal.authority_source == RemoteAuthoritySource::Unconfigured
        {
            warn!(
                target: "termul::web::auth",
                capability = capability.as_str(),
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
        self.check_rate_limit(peer)?;
        match self.verify_bearer(token) {
            Ok(principal) => {
                self.failures.lock().remove(&peer);
                Ok(principal)
            }
            Err(error) => {
                let reported = self.record_failure(peer).err().unwrap_or(error);
                warn!(
                    target: "termul::web::auth",
                    auth_class = "bearer",
                    stable_code = reported.code(),
                    "remote authentication failed"
                );
                Err(reported)
            }
        }
    }

    fn check_rate_limit(&self, peer: IpAddr) -> Result<(), RemoteAuthError> {
        let now = Instant::now();
        let mut failures = self.failures.lock();
        if let Some(state) = failures.get_mut(&peer) {
            if state.locked_until.is_some_and(|until| until > now) {
                return Err(RemoteAuthError::RateLimited);
            }
            if now.duration_since(state.window_started) >= FAILURE_WINDOW {
                failures.remove(&peer);
            }
        }
        Ok(())
    }

    fn record_failure(&self, peer: IpAddr) -> Result<(), RemoteAuthError> {
        let now = Instant::now();
        let mut failures = self.failures.lock();
        let state = failures.entry(peer).or_insert(FailureState {
            window_started: now,
            failures: 0,
            locked_until: None,
        });
        if now.duration_since(state.window_started) >= FAILURE_WINDOW {
            state.window_started = now;
            state.failures = 0;
            state.locked_until = None;
        }
        state.failures = state.failures.saturating_add(1);
        if state.failures > FAILURE_LIMIT {
            state.locked_until = Some(now + LOCKOUT);
            Err(RemoteAuthError::RateLimited)
        } else {
            Ok(())
        }
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
    let Some(capability) = protected_capability(request.method(), request.uri().path()) else {
        return next.run(request).await;
    };
    let started = Instant::now();
    let request_type = format!("{} {}", request.method(), request.uri().path());
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
            &request_type,
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
                &request_type,
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
            &request_type,
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
        &request_type,
        capability,
        stable_code,
        response.status(),
        started.elapsed(),
    );
    response
}

fn log_boundary_outcome(
    request_type: &str,
    capability: RemoteCapability,
    stable_code: &str,
    status: StatusCode,
    duration: Duration,
) {
    if status.is_client_error() || status.is_server_error() {
        warn!(
            target: "termul::web::auth",
            request_type,
            auth_class = "bearer",
            capability = capability.as_str(),
            stable_code,
            http_status = status.as_u16(),
            duration_ms = duration.as_millis(),
            "remote boundary request rejected"
        );
    } else {
        info!(
            target: "termul::web::auth",
            request_type,
            auth_class = "bearer",
            capability = capability.as_str(),
            stable_code,
            http_status = status.as_u16(),
            duration_ms = duration.as_millis(),
            "remote boundary request completed"
        );
    }
}

fn protected_capability(method: &Method, path: &str) -> Option<RemoteCapability> {
    if path == "/ws" || path == "/health" {
        return None;
    }
    let protected = [
        "/projects",
        "/mcp-servers",
        "/fs/",
        "/git/",
        "/search/",
        "/skills",
        "/log/",
        "/shells",
        "/workspace/",
        "/conversations",
        "/conversation-recovery/",
        "/acp/",
        "/worktree/",
        "/terminal/ws",
    ]
    .iter()
    .any(|prefix| path == *prefix || path.starts_with(prefix));
    if !protected {
        return None;
    }
    if path.starts_with("/conversation-recovery/") {
        Some(RemoteCapability::RecoveryInspect)
    } else if *method == Method::GET {
        Some(RemoteCapability::Read)
    } else {
        Some(RemoteCapability::Mutate)
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
        "CONVERSATION_RECOVERY_REQUIRED" | "LEGACY_COMPATIBILITY_READ_ONLY" => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    const TOKEN: &str = "test-remote-access-token";

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
    fn sixth_failed_attempt_is_rate_limited() {
        let authority = authority();
        let peer = IpAddr::from([192, 0, 2, 10]);
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
        assert_eq!(
            authority.verify_bearer_for_peer(TOKEN, peer).unwrap_err(),
            RemoteAuthError::RateLimited
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
