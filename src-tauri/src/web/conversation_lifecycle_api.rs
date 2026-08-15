//! HTTP adapters for canonical Conversation binding and tombstone lifecycle operations.

use std::net::SocketAddr;

use axum::{
    body::Bytes,
    extract::{ConnectInfo, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use tracing::warn;

use crate::conversation::{
    ConversationApplicationService, ConversationId, ConversationLifecycleOutcome,
    PrepareConversationRequest,
};
use crate::web::fs_api::IpcBody;
use crate::web::sink::AcpEvent;
use crate::web::ws::AppState;
use crate::web::EventSink;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevisionRequest {
    expected_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplaceRequest {
    expected_revision: u64,
    request: PrepareConversationRequest,
}

pub async fn detach(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(conversation_id): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    mutate_revision(state, peer, conversation_id, body, Mutation::Detach).await
}

pub async fn rebind(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(conversation_id): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    mutate_revision(state, peer, conversation_id, body, Mutation::Rebind).await
}

pub async fn suspend(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(conversation_id): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    mutate_revision(state, peer, conversation_id, body, Mutation::Suspend).await
}

pub async fn delete(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(conversation_id): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    mutate_revision(state, peer, conversation_id, body, Mutation::Delete).await
}

pub async fn replace(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(conversation_id): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    if !peer.ip().is_loopback() {
        return forbidden();
    }
    let conversation_id = match parse_id(&conversation_id) {
        Ok(value) => value,
        Err((code, detail)) => return failure(code, detail),
    };
    let request: ReplaceRequest = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => return validation(error.to_string()),
    };
    let service = match application(&state) {
        Ok(service) => service,
        Err((code, detail)) => return failure(code, detail),
    };
    respond(
        &state,
        conversation_id,
        service
            .replace_binding(conversation_id, request.request, request.expected_revision)
            .await,
    )
}

#[derive(Clone, Copy)]
enum Mutation {
    Detach,
    Rebind,
    Suspend,
    Delete,
}

async fn mutate_revision(
    state: AppState,
    peer: SocketAddr,
    conversation_id: String,
    body: Bytes,
    mutation: Mutation,
) -> (StatusCode, Json<IpcBody<ConversationLifecycleOutcome>>) {
    if !peer.ip().is_loopback() {
        return forbidden();
    }
    let conversation_id = match parse_id(&conversation_id) {
        Ok(value) => value,
        Err((code, detail)) => return failure(code, detail),
    };
    let request: RevisionRequest = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => return validation(error.to_string()),
    };
    let service = match application(&state) {
        Ok(service) => service,
        Err((code, detail)) => return failure(code, detail),
    };
    let result = match mutation {
        Mutation::Detach => {
            service
                .detach_binding(conversation_id, request.expected_revision)
                .await
        }
        Mutation::Rebind => {
            service
                .rebind_binding(conversation_id, request.expected_revision)
                .await
        }
        Mutation::Suspend => {
            service
                .suspend_binding(conversation_id, request.expected_revision)
                .await
        }
        Mutation::Delete => {
            service
                .delete_conversation(conversation_id, request.expected_revision)
                .await
        }
    };
    respond(&state, conversation_id, result)
}

fn application(
    state: &AppState,
) -> Result<std::sync::Arc<ConversationApplicationService>, (String, String)> {
    state.conversation.clone().ok_or_else(|| {
        (
            "CONVERSATION_SERVICE_UNAVAILABLE".to_string(),
            "bootstrap-published Conversation application service is unavailable".to_string(),
        )
    })
}

fn respond(
    state: &AppState,
    conversation_id: ConversationId,
    result: crate::conversation::application::Result<ConversationLifecycleOutcome>,
) -> (StatusCode, Json<IpcBody<ConversationLifecycleOutcome>>) {
    match result {
        Ok(outcome) => {
            state.relay.emit(&AcpEvent {
                sid: None,
                type_: "conversation_lifecycle",
                payload: serde_json::to_value(&outcome)
                    .expect("Conversation lifecycle outcome serializes"),
            });
            (StatusCode::OK, Json(IpcBody::ok(outcome)))
        }
        Err(error) => {
            warn!(
                target: "termul::web::conversation_lifecycle_api",
                conversation_id = %conversation_id,
                code = %error.code,
                operation = error.operation,
                "conversation lifecycle mutation failed"
            );
            failure(error.code, error.detail)
        }
    }
}

fn parse_id(value: &str) -> Result<ConversationId, (String, String)> {
    ConversationId::parse_path_component(value)
        .map_err(|error| ("CONVERSATION_INVALID_ID".to_string(), error.to_string()))
}

fn validation(detail: String) -> (StatusCode, Json<IpcBody<ConversationLifecycleOutcome>>) {
    failure(
        "VALIDATION_ERROR".to_string(),
        format!("payload validation failed: {detail}"),
    )
}

fn forbidden() -> (StatusCode, Json<IpcBody<ConversationLifecycleOutcome>>) {
    failure(
        "FORBIDDEN".to_string(),
        "Conversation lifecycle mutation routes are localhost-only".to_string(),
    )
}

fn failure(
    code: String,
    detail: String,
) -> (StatusCode, Json<IpcBody<ConversationLifecycleOutcome>>) {
    (
        StatusCode::OK,
        Json(IpcBody::<ConversationLifecycleOutcome>::err(detail, code)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::AcpManager;
    use crate::conversation::contracts::{
        parse_created_at_utc, AgentSessionBinding, AgentSessionBindingState, ConversationCreator,
        ConversationLifecycleState, ConversationRecordV2, CreationPartition, ExecutionTarget,
        AGENT_SESSION_BINDING_SCHEMA_VERSION, CONVERSATION_SCHEMA_VERSION,
    };
    use crate::conversation::migration::{
        MigrationHostMode, MigrationMapV1, MigrationPhase, ReaderPrecedence,
        MIGRATION_MAP_SCHEMA_VERSION,
    };
    use crate::conversation::{
        ConversationApplicationService, ConversationCreationService, ConversationLifecycleService,
        ConversationLocator, ConversationPersistenceAdapter, ConversationReader,
        ConversationRepository, SessionWorkspaceLocator, SessionWorkspaceService,
    };
    use crate::web::ws::HistoryMode;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::post;
    use std::sync::Arc;
    use tower::ServiceExt;
    use uuid::Uuid;

    const ID: &str = "018f7a1c-1b4d-7c8a-9f01-0123456789ab";

    async fn state() -> (tempfile::TempDir, AppState, u64) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let private = base.join("state/conversations/v2");
        let visible = base.join("visible");
        std::fs::create_dir_all(&visible).unwrap();
        let (repository, _) = ConversationRepository::open(private.clone()).unwrap();
        let id = ConversationId::parse(ID).unwrap();
        let created_at = parse_created_at_utc("2026-08-15T09:45:15.123Z").unwrap();
        let workspace = visible.join("sessions/2026/08/15").join(ID);
        std::fs::create_dir_all(&workspace).unwrap();
        repository
            .create_conversation(ConversationRecordV2 {
                schema_version: CONVERSATION_SCHEMA_VERSION,
                conversation_id: id,
                created_at_utc: created_at,
                creation_partition: CreationPartition::from_created_at(created_at),
                workspace_cwd: workspace.to_string_lossy().into_owned(),
                execution_target: ExecutionTarget::Workspace,
                project_attachment: None,
                lifecycle_state: ConversationLifecycleState::Ready,
                last_seq: 0,
                created_by: ConversationCreator::Termul,
            })
            .await
            .unwrap();
        repository
            .bind_agent_session(
                id,
                AgentSessionBinding {
                    schema_version: AGENT_SESSION_BINDING_SCHEMA_VERSION,
                    binding_id: Uuid::new_v4(),
                    agent_session_id: "opaque/http".to_string(),
                    runtime_agent_id: "agent-http".to_string(),
                    stable_agent_namespace: "config:http".to_string(),
                    execution_cwd: workspace.to_string_lossy().into_owned(),
                    bound_at_utc: chrono::Utc::now(),
                    state: AgentSessionBindingState::Active,
                },
                chrono::Utc::now(),
            )
            .await
            .unwrap();
        let creation = Arc::new(
            ConversationCreationService::new(
                Arc::clone(&repository),
                ConversationLocator::new(private).unwrap(),
                SessionWorkspaceLocator::new(visible).unwrap(),
            )
            .unwrap(),
        );
        let reader = Arc::new(ConversationReader::new(
            Arc::clone(&repository),
            Default::default(),
            ReaderPrecedence::ConversationV2Only,
        ));
        let persistence = Arc::new(ConversationPersistenceAdapter::new(
            Arc::clone(&repository),
            Arc::clone(&reader),
        ));
        let acp = Arc::new(AcpManager::with_conversation_services(
            vec![],
            creation,
            persistence,
        ));
        let pty = crate::web::test_pty_manager();
        acp.set_pty_manager(&pty);
        let migration_map = MigrationMapV1 {
            schema_version: MIGRATION_MAP_SCHEMA_VERSION,
            operation_id: Uuid::new_v4(),
            entries: Vec::new(),
        };
        let conversation = Arc::new(ConversationApplicationService::new(
            reader,
            Arc::new(SessionWorkspaceService::new(Arc::clone(&repository))),
            &migration_map,
            MigrationHostMode::Standalone,
            MigrationPhase::Finalized,
            ReaderPrecedence::ConversationV2Only,
            0,
        ));
        conversation
            .attach_lifecycle(
                ConversationLifecycleService::from_manager(Arc::clone(&acp), Arc::clone(&pty))
                    .unwrap(),
            )
            .unwrap();
        let revision = repository.get_conversation(id).unwrap().last_seq;
        let state = AppState {
            acp,
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
            workspace_manifest: None,
            acp_catalog: None,
            acp_install: None,
        };
        (temp, state, revision)
    }

    fn router(state: AppState) -> axum::Router {
        axum::Router::new()
            .route(
                "/conversations/{conversationId}/lifecycle/detach",
                post(detach),
            )
            .route(
                "/conversations/{conversationId}/lifecycle/delete",
                post(delete),
            )
            .with_state(state)
    }

    async fn body(response: axum::response::Response) -> IpcBody<ConversationLifecycleOutcome> {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn detach_and_stale_last_seq_use_stable_camel_case_contract() {
        let (_temp, state, revision) = state().await;
        let app = router(state);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/conversations/{ID}/lifecycle/detach"))
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 3000))))
                    .body(Body::from(format!("{{\"expectedRevision\":{revision}}}")))
                    .unwrap(),
            )
            .await
            .unwrap();
        let detached = body(response).await;
        let value = serde_json::to_value(detached.data.unwrap()).unwrap();
        assert_eq!(value["action"], "detachBinding");
        assert_eq!(value["currentBinding"]["state"], "detached");

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/conversations/{ID}/lifecycle/delete"))
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 3000))))
                    .body(Body::from(format!("{{\"expectedRevision\":{revision}}}")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            body(response).await.code.as_deref(),
            Some("CONVERSATION_CONFLICT")
        );
    }
}
