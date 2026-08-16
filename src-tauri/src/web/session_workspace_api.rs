//! HTTP facade for revisioned per-Conversation SessionWorkspace and recovery actions.

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::Deserialize;
use tracing::warn;

use crate::conversation::migration::RecoveryAuthorizationClass;
use crate::conversation::{
    ConversationApplicationService, ConversationId, SessionWorkspaceLoadOutcome,
    SessionWorkspaceV1, SessionWorkspaceWriteOutcome,
};
use crate::web::auth::{status_for_code, RemoteAccessAuthority, RemoteCapability, RemotePrincipal};
use crate::web::fs_api::IpcBody;
use crate::web::ws::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriteRequest {
    pub based_revision: Option<u64>,
    pub workspace: SessionWorkspaceV1,
}

fn service(
    state: &AppState,
) -> Result<std::sync::Arc<ConversationApplicationService>, (&'static str, String)> {
    state.conversation.clone().ok_or_else(|| {
        (
            "SESSION_WORKSPACE_UNAVAILABLE",
            "bootstrap-published Conversation application service is unavailable".to_string(),
        )
    })
}

pub async fn get(
    State(state): State<AppState>,
    Extension(authority): Extension<Arc<RemoteAccessAuthority>>,
    Extension(principal): Extension<RemotePrincipal>,
    Path(conversation_id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) =
        require::<SessionWorkspaceLoadOutcome>(&authority, &principal, RemoteCapability::Read)
    {
        return response;
    }
    let conversation_id = match ConversationId::parse_path_component(&conversation_id) {
        Ok(value) => value,
        Err(error) => {
            return (
                status_for_code("CONVERSATION_INVALID_ID"),
                Json(IpcBody::<SessionWorkspaceLoadOutcome>::err(
                    error.to_string(),
                    "CONVERSATION_INVALID_ID",
                )),
            )
        }
    };
    let service = match service(&state) {
        Ok(service) => service,
        Err((code, detail)) => {
            return (
                status_for_code(code),
                Json(IpcBody::<SessionWorkspaceLoadOutcome>::err(detail, code)),
            )
        }
    };
    match service.get_workspace(conversation_id).await {
        Ok(outcome) => {
            let status = if matches!(
                outcome,
                SessionWorkspaceLoadOutcome::RecoveryRequired { .. }
            ) {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::OK
            };
            (status, Json(IpcBody::ok(outcome)))
        }
        Err(error) => {
            warn!(
                target: "termul::web::session_workspace_api",
                conversation_id = %conversation_id,
                code = %error.code,
                "workspace get failed"
            );
            (
                status_for_code(&error.code),
                Json(IpcBody::<SessionWorkspaceLoadOutcome>::err(
                    error.detail,
                    error.code,
                )),
            )
        }
    }
}

pub async fn write(
    State(state): State<AppState>,
    Extension(authority): Extension<Arc<RemoteAccessAuthority>>,
    Extension(principal): Extension<RemotePrincipal>,
    Path(conversation_id): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(response) =
        require::<SessionWorkspaceWriteOutcome>(&authority, &principal, RemoteCapability::Mutate)
    {
        return response;
    }
    let conversation_id = match ConversationId::parse_path_component(&conversation_id) {
        Ok(value) => value,
        Err(error) => {
            return (
                status_for_code("CONVERSATION_INVALID_ID"),
                Json(IpcBody::<SessionWorkspaceWriteOutcome>::err(
                    error.to_string(),
                    "CONVERSATION_INVALID_ID",
                )),
            )
        }
    };
    let request: WriteRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return (
                status_for_code("VALIDATION_ERROR"),
                Json(IpcBody::<SessionWorkspaceWriteOutcome>::err(
                    format!("payload validation failed: {error}"),
                    "VALIDATION_ERROR",
                )),
            )
        }
    };
    let service = match service(&state) {
        Ok(service) => service,
        Err((code, detail)) => {
            return (
                status_for_code(code),
                Json(IpcBody::<SessionWorkspaceWriteOutcome>::err(detail, code)),
            )
        }
    };
    match service
        .write_workspace(conversation_id, request.based_revision, request.workspace)
        .await
    {
        Ok(outcome) => {
            let status = if matches!(outcome, SessionWorkspaceWriteOutcome::Conflict { .. }) {
                StatusCode::CONFLICT
            } else if matches!(
                outcome,
                SessionWorkspaceWriteOutcome::RecoveryRequired { .. }
            ) {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::OK
            };
            (status, Json(IpcBody::ok(outcome)))
        }
        Err(error) => {
            warn!(
                target: "termul::web::session_workspace_api",
                conversation_id = %conversation_id,
                code = %error.code,
                "workspace write failed"
            );
            (
                status_for_code(&error.code),
                Json(IpcBody::<SessionWorkspaceWriteOutcome>::err(
                    error.detail,
                    error.code,
                )),
            )
        }
    }
}

pub async fn resolve_recovery(
    State(state): State<AppState>,
    Extension(authority): Extension<Arc<RemoteAccessAuthority>>,
    Extension(principal): Extension<RemotePrincipal>,
    body: Bytes,
) -> impl IntoResponse {
    let request: crate::conversation::migration::ResolveRecoveryItemRequest =
        match serde_json::from_slice(&body) {
            Ok(request) => request,
            Err(error) => {
                return (
                    status_for_code("VALIDATION_ERROR"),
                    Json(IpcBody::<
                        crate::conversation::migration::RecoveryActionResult,
                    >::err(
                        format!("payload validation failed: {error}"),
                        "VALIDATION_ERROR",
                    )),
                )
            }
        };
    let capability = if request.action.authorization() == RecoveryAuthorizationClass::Mutation {
        RemoteCapability::Mutate
    } else {
        RemoteCapability::RecoveryInspect
    };
    if let Err(response) = require::<crate::conversation::migration::RecoveryActionResult>(
        &authority, &principal, capability,
    ) {
        return response;
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err((code, detail)) => {
            return (
                status_for_code(code),
                Json(IpcBody::<
                    crate::conversation::migration::RecoveryActionResult,
                >::err(detail, code)),
            )
        }
    };
    match service.resolve_recovery_item(request).await {
        Ok(mut outcome) => {
            outcome.source_paths.clear();
            outcome.source_sha256.clear();
            outcome.candidate_facts.clear();
            outcome.provenance.clear();
            (StatusCode::OK, Json(IpcBody::ok(outcome)))
        }
        Err(error) => (
            status_for_code(&error.code),
            Json(IpcBody::<
                crate::conversation::migration::RecoveryActionResult,
            >::err(error.detail, error.code)),
        ),
    }
}

fn require<T>(
    authority: &RemoteAccessAuthority,
    principal: &RemotePrincipal,
    capability: RemoteCapability,
) -> Result<(), (StatusCode, Json<IpcBody<T>>)> {
    authority.authorize(principal, capability).map_err(|error| {
        (
            error.status(),
            Json(IpcBody::err(error.to_string(), error.code())),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::WorkspaceManifestService;
    use crate::conversation::contracts::{
        parse_created_at_utc, ConversationCreator, ConversationLifecycleState,
        ConversationRecordV2, CreationPartition, ExecutionTarget, CONVERSATION_SCHEMA_VERSION,
    };
    use crate::conversation::migration::{
        MigrationHostMode, MigrationMapV1, MigrationPhase, ReaderPrecedence,
        MIGRATION_MAP_SCHEMA_VERSION,
    };
    use crate::conversation::{
        ConversationMutation, ConversationReader, ConversationRepository, ConversationWriter,
        LegacyConversationReader, SessionWorkspaceProjectionState, SessionWorkspaceService,
    };
    use crate::web::ws::HistoryMode;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use axum::routing::{get, post};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tower::ServiceExt;

    const ID: &str = "018f7a1c-1b4d-7c8a-9f01-0123456789ab";

    async fn state() -> (tempfile::TempDir, Arc<ConversationRepository>, AppState) {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().canonicalize().unwrap().join("state");
        let (repository, _) =
            ConversationRepository::open(state_root.join("conversations/v2")).unwrap();
        let writer = ConversationWriter::for_test(Arc::clone(&repository));
        let created_at_utc = parse_created_at_utc("2026-08-15T09:45:15.123Z").unwrap();
        writer
            .create_conversation(
                ConversationRecordV2 {
                    schema_version: CONVERSATION_SCHEMA_VERSION,
                    conversation_id: ConversationId::parse(ID).unwrap(),
                    created_at_utc,
                    creation_partition: CreationPartition::from_created_at(created_at_utc),
                    workspace_cwd: "/visible/session".to_string(),
                    execution_target: ExecutionTarget::Workspace,
                    project_attachment: None,
                    lifecycle_state: ConversationLifecycleState::Ready,
                    last_seq: 0,
                    created_by: ConversationCreator::Termul,
                },
                ConversationMutation::CreateConversation,
            )
            .await
            .unwrap();
        let legacy =
            WorkspaceManifestService::open_read_only(state_root.join("workspace-manifests"))
                .await
                .unwrap();
        let reader = Arc::new(ConversationReader::new(
            Arc::clone(&repository),
            LegacyConversationReader::default(),
            ReaderPrecedence::ConversationV2Only,
        ));
        let migration_map = MigrationMapV1 {
            schema_version: MIGRATION_MAP_SCHEMA_VERSION,
            operation_id: uuid::Uuid::new_v4(),
            entries: Vec::new(),
        };
        let workspace = Arc::new(SessionWorkspaceService::new(Arc::clone(&writer)));
        let conversation = Arc::new(ConversationApplicationService::new(
            reader,
            writer,
            workspace,
            &migration_map,
            MigrationHostMode::Standalone,
            MigrationPhase::Finalized,
            ReaderPrecedence::ConversationV2Only,
        ));
        let pty = crate::web::test_pty_manager();
        (
            temp,
            repository,
            AppState {
                acp: Arc::new(crate::acp::AcpManager::new(vec![])),
                terminal_events: pty.terminal_events(),
                cwd_tracker: pty.cwd_tracker(),
                git_tracker: pty.git_tracker(),
                exit_code_tracker: pty.exit_code_tracker(),
                pty,
                relay: Arc::new(crate::web::sink::WsRelaySink::new()),
                registry: Arc::new(crate::web::project_registry::ProjectRegistry::new()),
                registry_persistence: None,
                projects_file: None,
                history_mode: HistoryMode::LiveOnly,
                conversation: Some(conversation),
                project_root: Arc::new(parking_lot::RwLock::new(std::env::temp_dir())),
                workspace_manifest: Some(legacy),
                acp_catalog: None,
                acp_install: None,
            },
        )
    }

    fn router(state: AppState) -> axum::Router {
        let authority = Arc::new(RemoteAccessAuthority::for_tests("workspace-api-token"));
        let principal = authority.verify_bearer("workspace-api-token").unwrap();
        axum::Router::new()
            .route(
                "/conversations/{conversationId}/workspace",
                get(super::get).post(write),
            )
            .route("/conversation-recovery/resolve", post(resolve_recovery))
            .with_state(state)
            .layer(Extension(principal))
            .layer(Extension(authority))
    }

    fn workspace() -> SessionWorkspaceV1 {
        SessionWorkspaceV1 {
            schema_version: 1,
            conversation_id: ConversationId::parse(ID).unwrap(),
            revision: 0,
            updated_at_utc: String::new(),
            update_identity: Some("web-test".to_string()),
            topology: None,
            active_pane_id: None,
            resources: Vec::new(),
            projection_state: SessionWorkspaceProjectionState::Native,
        }
    }

    async fn body<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn loopback() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 3000)))
    }

    fn seed_recovery(
        repository: &ConversationRepository,
    ) -> (
        std::path::PathBuf,
        crate::conversation::migration::RecoveryItemV1,
    ) {
        use crate::conversation::migration::{
            RecoveryItemV1, RecoveryKind, RecoveryProvenanceV1, RecoveryQueueV1, RecoverySeverity,
        };
        let item = RecoveryItemV1::new(
            RecoveryKind::AmbiguousWorkspaceManifest,
            RecoverySeverity::Warning,
            vec!["legacy_workspace_manifests/0/shared.json".to_string()],
            vec![ConversationId::parse(ID).unwrap()],
            vec!["e".repeat(64)],
            vec![serde_json::json!({"candidate":"preserved"})],
            vec![RecoveryProvenanceV1 {
                source_kind: "legacy_workspace_manifests".to_string(),
                relative_path: "legacy_workspace_manifests/0/shared.json".to_string(),
                sha256: "e".repeat(64),
                preserved_read_only: true,
            }],
        );
        let state_root = repository
            .root()
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap();
        let operation_dir = state_root
            .join("conversation-migrations")
            .join("workspace-recovery-v1");
        RecoveryQueueV1::new(uuid::Uuid::new_v4(), vec![item.clone()])
            .persist(&operation_dir)
            .unwrap();
        (operation_dir, item)
    }

    async fn post_recovery(
        app: axum::Router,
        request: serde_json::Value,
    ) -> IpcBody<crate::conversation::migration::RecoveryActionResult> {
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/conversation-recovery/resolve")
                    .header("content-type", "application/json")
                    .extension(loopback())
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        body(response).await
    }

    #[tokio::test]
    async fn workspace_get_write_conflict_and_forbidden_contract() {
        let (_temp, _repository, state) = state().await;
        let app = router(state);
        let get_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/conversations/{ID}/workspace"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let missing: IpcBody<SessionWorkspaceLoadOutcome> = body(get_response).await;
        assert!(
            matches!(
                missing.data,
                Some(SessionWorkspaceLoadOutcome::Missing { .. })
            ),
            "unexpected get body: {missing:?}"
        );

        let request = serde_json::to_vec(&serde_json::json!({
            "basedRevision":null,
            "workspace":workspace()
        }))
        .unwrap();
        let write_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/conversations/{ID}/workspace"))
                    .header("content-type", "application/json")
                    .extension(loopback())
                    .body(Body::from(request.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let updated: IpcBody<SessionWorkspaceWriteOutcome> = body(write_response).await;
        assert!(matches!(
            updated.data,
            Some(SessionWorkspaceWriteOutcome::Updated { revision: 1, .. })
        ));

        let conflict_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/conversations/{ID}/workspace"))
                    .header("content-type", "application/json")
                    .extension(loopback())
                    .body(Body::from(request.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(conflict_response.status(), StatusCode::CONFLICT);
        let conflict: IpcBody<SessionWorkspaceWriteOutcome> = body(conflict_response).await;
        assert!(matches!(
            conflict.data,
            Some(SessionWorkspaceWriteOutcome::Conflict {
                current_revision: 1,
                ..
            })
        ));

        let forbidden_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/conversations/{ID}/workspace"))
                    .header("content-type", "application/json")
                    .extension(ConnectInfo(SocketAddr::from(([192, 168, 1, 5], 3000))))
                    .body(Body::from(request))
                    .unwrap(),
            )
            .await
            .unwrap();
        let proxied: IpcBody<SessionWorkspaceWriteOutcome> = body(forbidden_response).await;
        assert!(
            proxied.success,
            "authenticated proxy requests must not rely on peer IP"
        );
    }

    #[tokio::test]
    async fn recovery_actions_execute_exact_shared_status_and_workspace_effects() {
        let cases = [
            (
                "inspect",
                serde_json::json!({}),
                None,
                "unresolved",
                1_u64,
                false,
            ),
            (
                "associateConversation",
                serde_json::json!({"conversationId":ID}),
                Some("21aee10a-56b8-4624-a5e7-586c25dc8d1f"),
                "resolvedAssociated",
                2,
                false,
            ),
            (
                "startEmptyWorkspace",
                serde_json::json!({"conversationId":ID,"expectedWorkspaceRevision":null}),
                Some("d70c2b93-71bc-4df0-85a5-15bd1b7cf452"),
                "resolvedStartedEmpty",
                2,
                true,
            ),
            (
                "dismissPreservedSource",
                serde_json::json!({"reasonCode":"deferLegacyProjection"}),
                Some("b025313d-df5d-4254-af4f-535b47ea570f"),
                "dismissedPreserved",
                2,
                false,
            ),
        ];
        for (
            action,
            payload,
            idempotency_key,
            expected_status,
            expected_revision,
            workspace_changed,
        ) in cases
        {
            let (_temp, repository, state) = state().await;
            let (operation_dir, item) = seed_recovery(&repository);
            let mut request = serde_json::json!({
                "recoveryId":item.recovery_id,
                "expectedRevision":item.revision,
                "action":action,
                "payload":payload
            });
            if let Some(key) = idempotency_key {
                request
                    .as_object_mut()
                    .unwrap()
                    .insert("idempotencyKey".to_string(), serde_json::json!(key));
            }
            let response = post_recovery(router(state), request).await;
            assert!(response.success, "{action} failed: {:?}", response.error);
            let result = response.data.unwrap();
            assert_eq!(
                serde_json::to_value(result.action).unwrap(),
                expected_status_for_action(action)
            );
            assert_eq!(
                serde_json::to_value(result.status).unwrap(),
                expected_status
            );
            assert_eq!(result.recovery_revision, expected_revision);
            assert_eq!(result.workspace_changed, workspace_changed);
            assert!(result.source_paths.is_empty());
            assert!(result.source_sha256.is_empty());
            assert!(result.candidate_facts.is_empty());
            assert!(result.provenance.is_empty());
            let persisted: crate::conversation::migration::RecoveryQueueV1 =
                serde_json::from_slice(
                    &std::fs::read(
                        operation_dir.join(crate::conversation::migration::RECOVERY_ITEMS_FILE),
                    )
                    .unwrap(),
                )
                .unwrap();
            assert_eq!(persisted.items[0].revision, expected_revision);
            let workspace_path = repository
                .root()
                .join("2026/08/15")
                .join(ID)
                .join("workspace.json");
            assert_eq!(workspace_path.exists(), workspace_changed);
            if workspace_changed {
                let workspace: SessionWorkspaceV1 =
                    serde_json::from_slice(&std::fs::read(workspace_path).unwrap()).unwrap();
                assert_eq!(workspace.revision, 1);
                assert!(workspace.resources.is_empty());
            }
        }
    }

    fn expected_status_for_action(action: &str) -> serde_json::Value {
        serde_json::Value::String(action.to_string())
    }

    #[test]
    fn recovery_action_contract() {
        let source = include_str!("../../../src/shared/types/conversation-recovery.types.ts");
        for action in [
            "inspect",
            "associateConversation",
            "startEmptyWorkspace",
            "dismissPreservedSource",
        ] {
            assert!(source.contains(action));
        }
        assert!(!source.contains("associate_conversation"));
    }
}
