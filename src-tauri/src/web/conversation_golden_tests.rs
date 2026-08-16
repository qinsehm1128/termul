use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::middleware;
use axum::routing::{get, post};
use axum::Extension;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use super::conversation_api;
use super::ws::{dispatch_conversation_golden_request, AppState, HistoryMode};
use crate::conversation::contracts::{
    parse_created_at_utc, AgentSessionBinding, AgentSessionBindingState, ConversationCreator,
    ConversationLifecycleState, ConversationRecordV2, CreationPartition, ExecutionTarget,
    ProjectAttachment, AGENT_SESSION_BINDING_SCHEMA_VERSION, CONVERSATION_SCHEMA_VERSION,
};
use crate::conversation::migration::{
    CreatedAtSource, IdentityDecision, MigrationHostMode, MigrationMapEntryV1, MigrationMapV1,
    MigrationPhase, ReaderPrecedence, RecoveryItemV1, RecoveryKind, RecoveryProvenanceV1,
    RecoveryQueueV1, RecoverySeverity, MIGRATION_MAP_SCHEMA_VERSION,
};
use crate::conversation::{
    AgentBindingResult, AgentLifecycleProviderError, ConversationAgentLifecycle,
    ConversationApplicationService, ConversationCreationService, ConversationId,
    ConversationLifecycleService, ConversationLocator, ConversationMutation, ConversationReader,
    ConversationRepository, ConversationWriter, LegacyConversationReader, PreparedConversation,
    SessionWorkspaceLocator, SessionWorkspaceService, TerminalResourceInspector,
};

const ID: &str = "018f7a1c-1b4d-7c8a-9f01-0123456789ab";
const BINDING_ID: &str = "33333333-3333-4333-8333-333333333333";

type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Default)]
struct GoldenLifecycleProvider;

impl ConversationAgentLifecycle for GoldenLifecycleProvider {
    fn owns_session<'a>(&'a self, _binding: &'a AgentSessionBinding) -> ProviderFuture<'a, bool> {
        Box::pin(async { true })
    }

    fn suspend<'a>(
        &'a self,
        _binding: &'a AgentSessionBinding,
    ) -> ProviderFuture<'a, std::result::Result<(), AgentLifecycleProviderError>> {
        Box::pin(async { Ok(()) })
    }

    fn replace<'a>(
        &'a self,
        _previous_binding: &'a AgentSessionBinding,
        _prepared: &'a PreparedConversation,
    ) -> ProviderFuture<'a, std::result::Result<AgentBindingResult, AgentLifecycleProviderError>>
    {
        Box::pin(async {
            Ok(AgentBindingResult {
                agent_session_id: "opaque/golden/replacement".to_string(),
                runtime_agent_id: "runtime-golden-replacement".to_string(),
                stable_agent_namespace: "config:golden-replacement".to_string(),
            })
        })
    }

    fn abort_replacement<'a>(
        &'a self,
        _binding: &'a AgentSessionBinding,
    ) -> ProviderFuture<'a, std::result::Result<(), AgentLifecycleProviderError>> {
        Box::pin(async { Ok(()) })
    }

    fn register_binding(&self, _agent_session_id: &str, _conversation_id: ConversationId) {}
}

struct NoLiveTerminals;

impl TerminalResourceInspector for NoLiveTerminals {
    fn is_live(&self, _terminal_id: &str) -> bool {
        false
    }
}

struct GoldenFixture {
    _temp: tempfile::TempDir,
    private_root: PathBuf,
    visible_root: PathBuf,
    workspace_cwd: String,
    repository: Arc<ConversationRepository>,
    writer: Arc<ConversationWriter>,
    service: Arc<ConversationApplicationService>,
    state: AppState,
}

async fn fixture() -> GoldenFixture {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let private_root = base.join("state/conversations/v2");
    let visible_root = base.join("visible");
    let visible_workspace = visible_root.join("2026/08/15").join(ID);
    std::fs::create_dir_all(&visible_workspace).unwrap();
    let (repository, _) = ConversationRepository::open(private_root.clone()).unwrap();
    let writer = ConversationWriter::for_test(Arc::clone(&repository));
    let conversation_id = ConversationId::parse(ID).unwrap();
    let created_at = parse_created_at_utc("2026-08-15T09:45:15.123Z").unwrap();
    writer
        .create_conversation(
            ConversationRecordV2 {
                schema_version: CONVERSATION_SCHEMA_VERSION,
                conversation_id,
                created_at_utc: created_at,
                creation_partition: CreationPartition::from_created_at(created_at),
                workspace_cwd: visible_workspace.to_string_lossy().into_owned(),
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
    let map = MigrationMapV1 {
        schema_version: MIGRATION_MAP_SCHEMA_VERSION,
        operation_id: Uuid::new_v4(),
        entries: vec![MigrationMapEntryV1 {
            source_key: "legacy_chat_history:0:payloads/history-one.json".to_string(),
            legacy_storage_key: Some("storage-one".to_string()),
            legacy_agent_session_id: Some("agent-one".to_string()),
            conversation_id,
            identity_decision: IdentityDecision::AllocatedInvalidUuid,
            created_at_source: Some(CreatedAtSource::HostMetadata),
            source_record_sha256: "a".repeat(64),
        }],
    };
    let reader = Arc::new(ConversationReader::new(
        Arc::clone(&repository),
        LegacyConversationReader::default(),
        ReaderPrecedence::ConversationV2Only,
    ));
    let workspace = Arc::new(SessionWorkspaceService::new(Arc::clone(&writer)));
    let service = Arc::new(ConversationApplicationService::new(
        reader,
        Arc::clone(&writer),
        workspace,
        &map,
        MigrationHostMode::Standalone,
        MigrationPhase::Finalized,
        ReaderPrecedence::ConversationV2Only,
    ));
    let pty = crate::web::test_pty_manager();
    let state = AppState {
        acp: Arc::new(crate::acp::AcpManager::new(vec![])),
        terminal_events: pty.terminal_events(),
        cwd_tracker: pty.cwd_tracker(),
        git_tracker: pty.git_tracker(),
        exit_code_tracker: pty.exit_code_tracker(),
        pty,
        relay: Arc::new(crate::web::sink::WsRelaySink::new()),
        registry: Arc::new(crate::web::ProjectRegistry::new()),
        registry_persistence: None,
        projects_file: None,
        history_mode: HistoryMode::LiveOnly,
        conversation: Some(Arc::clone(&service)),
        workspace_manifest: None,
        acp_catalog: None,
        acp_install: None,
        project_root: Arc::new(parking_lot::RwLock::new(std::env::temp_dir())),
    };
    GoldenFixture {
        _temp: temp,
        private_root,
        visible_root,
        workspace_cwd: visible_workspace.to_string_lossy().into_owned(),
        repository,
        writer,
        service,
        state,
    }
}

async fn fixture_with_lifecycle() -> GoldenFixture {
    let fixture = fixture().await;
    let conversation_id = ConversationId::parse(ID).unwrap();
    let created_at = parse_created_at_utc("2026-08-15T09:45:15.123Z").unwrap();
    let workspace_cwd = fixture
        .repository
        .get_conversation(conversation_id)
        .unwrap()
        .workspace_cwd;
    fixture
        .writer
        .bind_agent_session(
            conversation_id,
            AgentSessionBinding {
                schema_version: AGENT_SESSION_BINDING_SCHEMA_VERSION,
                binding_id: Uuid::parse_str(BINDING_ID).unwrap(),
                agent_session_id: "opaque/golden/original".to_string(),
                runtime_agent_id: "runtime-golden-original".to_string(),
                stable_agent_namespace: "config:golden-original".to_string(),
                execution_cwd: workspace_cwd,
                bound_at_utc: created_at,
                state: AgentSessionBindingState::Active,
            },
            created_at,
        )
        .await
        .unwrap();
    let creation = Arc::new(
        ConversationCreationService::new(
            Arc::clone(&fixture.writer),
            ConversationLocator::new(fixture.private_root.clone()).unwrap(),
            SessionWorkspaceLocator::new(fixture.visible_root.clone()).unwrap(),
        )
        .unwrap(),
    );
    fixture
        .service
        .attach_lifecycle(ConversationLifecycleService::new(
            Arc::clone(&fixture.writer),
            creation,
            Arc::new(GoldenLifecycleProvider),
            Arc::new(NoLiveTerminals),
        ))
        .unwrap();
    fixture
}

fn app(state: AppState) -> axum::Router {
    let authority = Arc::new(crate::web::RemoteAccessAuthority::for_tests(
        "conversation-golden-token",
    ));
    let principal = authority
        .verify_bearer("conversation-golden-token")
        .unwrap();
    axum::Router::new()
        .route(
            "/conversations/host-status",
            get(conversation_api::host_status),
        )
        .route("/conversations", get(conversation_api::list))
        .route(
            "/conversations/resolve-legacy",
            post(conversation_api::resolve_legacy),
        )
        .route(
            "/conversation-recovery/resolve",
            post(conversation_api::resolve_recovery),
        )
        .route(
            "/conversations/{conversationId}/attach-project",
            post(conversation_api::attach_project),
        )
        .route(
            "/conversations/{conversationId}/detach-project",
            post(conversation_api::detach_project),
        )
        .route(
            "/conversations/{conversationId}/execution-target",
            post(conversation_api::update_execution_target),
        )
        .route(
            "/conversations/{conversationId}/lifecycle/detach",
            post(super::conversation_lifecycle_api::detach),
        )
        .with_state(state)
        .layer(Extension(principal))
        .layer(Extension(authority))
}

fn secured_app(state: AppState, authority: Arc<crate::web::RemoteAccessAuthority>) -> axum::Router {
    axum::Router::new()
        .route("/conversations", get(conversation_api::list))
        .route(
            "/conversations/{conversationId}",
            get(conversation_api::get),
        )
        .route(
            "/conversations/{conversationId}/attach-project",
            post(conversation_api::attach_project),
        )
        .route("/terminal/ws", get(super::terminal_ws::terminal_ws_upgrade))
        .with_state(state)
        .layer(middleware::from_fn(crate::web::auth::capability_middleware))
        .layer(Extension(authority))
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn normalize_workspace_paths(value: &mut Value, workspace_cwd: &str) {
    match value {
        Value::String(text) if text == workspace_cwd => {
            *text = "<workspaceCwd>".to_string();
        }
        Value::Array(values) => {
            for value in values {
                normalize_workspace_paths(value, workspace_cwd);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                normalize_workspace_paths(value, workspace_cwd);
            }
        }
        _ => {}
    }
}

fn normalized_workspace(mut value: Value, workspace_cwd: &str) -> Value {
    normalize_workspace_paths(&mut value, workspace_cwd);
    value
}

fn attachment(project_root: &std::path::Path) -> ProjectAttachment {
    ProjectAttachment {
        schema_version: 1,
        project_id: "project-golden".to_string(),
        attached_at_utc: parse_created_at_utc("2026-08-15T10:00:00.000Z").unwrap(),
        project_path_snapshot: project_root.to_string_lossy().into_owned(),
        worktree_path: None,
        worktree_branch: None,
    }
}

fn seed_recovery(repository: &ConversationRepository) -> RecoveryItemV1 {
    let item = RecoveryItemV1::new(
        RecoveryKind::AmbiguousWorkspaceManifest,
        RecoverySeverity::Warning,
        vec!["legacy_workspace_manifests/0/shared.json".to_string()],
        vec![ConversationId::parse(ID).unwrap()],
        vec!["e".repeat(64)],
        vec![json!({"candidate":"preserved"})],
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
    RecoveryQueueV1::new(Uuid::new_v4(), vec![item.clone()])
        .persist(
            &state_root
                .join("conversation-migrations")
                .join("workspace-recovery-v1"),
        )
        .unwrap();
    item
}

#[tokio::test]
async fn transport_golden_matrix() {
    let fixture = fixture().await;

    let tauri_status = crate::commands::conversation_host_status_inner(&fixture.service);
    assert!(tauri_status.success);
    let expected_status = serde_json::to_value(tauri_status.data.unwrap()).unwrap();
    let http_status = response_json(
        app(fixture.state.clone())
            .oneshot(
                Request::builder()
                    .uri("/conversations/host-status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(http_status["data"], expected_status);
    let mut authed = true;
    let ws_status = dispatch_conversation_golden_request(
        r#"{"id":"status-1","type":"conversation_host_status","payload":{}}"#,
        &mut authed,
        &fixture.service,
    )
    .await;
    assert_eq!(ws_status.payload.unwrap(), expected_status);

    let tauri = crate::commands::conversation_list_inner(&fixture.service);
    assert!(tauri.success);
    let tauri_data = serde_json::to_value(tauri.data.unwrap()).unwrap();

    let http_response = app(fixture.state.clone())
        .oneshot(
            Request::builder()
                .uri("/conversations")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let http = response_json(http_response).await;
    assert_eq!(http["success"], true);
    assert_eq!(http["data"], tauri_data);

    let mut authed = true;
    let ws = dispatch_conversation_golden_request(
        r#"{"id":"list-1","type":"list_conversations","payload":{}}"#,
        &mut authed,
        &fixture.service,
    )
    .await;
    assert!(ws.ok);
    assert_eq!(ws.payload.unwrap(), tauri_data);

    for (source_kind, value) in [
        ("legacyStorageKey", "storage-one"),
        ("legacyAgentSessionId", "agent-one"),
        ("legacyChatHistoryId", "history-one"),
    ] {
        let request = json!({"sourceKind":source_kind,"value":value});
        let tauri = crate::commands::conversation_resolve_legacy_id_inner(
            &fixture.service,
            request.clone(),
        );
        assert!(tauri.success);
        let expected = serde_json::to_value(tauri.data.unwrap()).unwrap();

        let http_response = app(fixture.state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/conversations/resolve-legacy")
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let http = response_json(http_response).await;
        assert_eq!(http["data"], expected, "sourceKind={source_kind}");

        let frame = json!({
            "id": format!("legacy-{source_kind}"),
            "type":"resolve_legacy_conversation_id",
            "payload":request
        });
        let ws =
            dispatch_conversation_golden_request(&frame.to_string(), &mut authed, &fixture.service)
                .await;
        assert_eq!(ws.payload.unwrap(), expected, "sourceKind={source_kind}");
    }
}

#[tokio::test]
async fn authenticated_http_and_terminal_boundaries_fail_closed_with_stable_statuses() {
    let fixture = fixture().await;
    let authority = Arc::new(crate::web::RemoteAccessAuthority::for_tests(
        "conversation-golden-token",
    ));

    for authorization in [None, Some("Bearer wrong-token")] {
        let mut builder = Request::builder().uri("/conversations");
        if let Some(value) = authorization {
            builder = builder.header("authorization", value);
        }
        let response = secured_app(fixture.state.clone(), Arc::clone(&authority))
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
        let body = response_json(response).await;
        assert_eq!(body["code"], "UNAUTHORIZED");
        assert!(body.get("data").is_none());
        assert!(!body.to_string().contains(
            &fixture
                .repository
                .get_conversation(ConversationId::parse(ID).unwrap())
                .unwrap()
                .workspace_cwd
        ));
    }

    let terminal = secured_app(fixture.state.clone(), Arc::clone(&authority))
        .oneshot(
            Request::builder()
                .uri("/terminal/ws")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(terminal.status(), axum::http::StatusCode::UNAUTHORIZED);

    let invalid = secured_app(fixture.state.clone(), Arc::clone(&authority))
        .oneshot(
            Request::builder()
                .uri("/conversations/not-a-conversation-id")
                .header("authorization", "Bearer conversation-golden-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(invalid).await["code"],
        "CONVERSATION_INVALID_ID"
    );

    let missing_id = "11111111-1111-4111-8111-111111111111";
    let missing = secured_app(fixture.state.clone(), Arc::clone(&authority))
        .oneshot(
            Request::builder()
                .uri(format!("/conversations/{missing_id}"))
                .header("authorization", "Bearer conversation-golden-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let missing_status = missing.status();
    let missing_body = response_json(missing).await;
    assert_eq!(
        missing_status,
        axum::http::StatusCode::NOT_FOUND,
        "unexpected missing response: {missing_body}"
    );
    assert_eq!(missing_body["code"], "CONVERSATION_NOT_FOUND");

    let project_root = fixture._temp.path().join("authenticated-project");
    std::fs::create_dir_all(&project_root).unwrap();
    let project_root = project_root.canonicalize().unwrap();
    let stale = secured_app(fixture.state, authority)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/conversations/{ID}/attach-project"))
                .header("authorization", "Bearer conversation-golden-token")
                .body(Body::from(
                    json!({
                        "expectedRevision":99,
                        "attachment":{
                            "schemaVersion":1,
                            "projectId":"project-golden",
                            "attachedAtUtc":"2026-08-15T10:00:00.000Z",
                            "projectPathSnapshot":project_root,
                            "worktreePath":null,
                            "worktreeBranch":null
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale.status(), axum::http::StatusCode::CONFLICT);
    assert_eq!(response_json(stale).await["code"], "CONVERSATION_CONFLICT");
}

#[tokio::test]
async fn aggregate_transport_golden_matrix() {
    let project_temp = tempfile::tempdir().unwrap();
    let project_root = project_temp.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();
    let project_root = std::fs::canonicalize(project_root).unwrap();
    let attachment = attachment(&project_root);
    let conversation_id = ConversationId::parse(ID).unwrap();

    let tauri_fixture = fixture().await;
    let tauri = crate::commands::conversation_attach_project_inner(
        &tauri_fixture.service,
        ID,
        0,
        serde_json::to_value(&attachment).unwrap(),
    )
    .await;
    let expected_attach = normalized_workspace(
        serde_json::to_value(tauri.data.unwrap()).unwrap(),
        &tauri_fixture.workspace_cwd,
    );

    let http_fixture = fixture().await;
    let http_workspace_cwd = http_fixture.workspace_cwd.clone();
    let http = response_json(
        app(http_fixture.state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/conversations/{ID}/attach-project"))
                    .body(Body::from(
                        json!({"expectedRevision":0,"attachment":attachment}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        normalized_workspace(http["data"].clone(), &http_workspace_cwd),
        expected_attach
    );

    let ws_fixture = fixture().await;
    let mut authed = true;
    let ws = dispatch_conversation_golden_request(
        &json!({
            "id":"attach-1",
            "type":"attach_project",
            "payload":{"conversationId":ID,"expectedRevision":0,"attachment":attachment}
        })
        .to_string(),
        &mut authed,
        &ws_fixture.service,
    )
    .await;
    assert_eq!(
        normalized_workspace(ws.payload.unwrap(), &ws_fixture.workspace_cwd),
        expected_attach
    );

    let target = ExecutionTarget::ProjectRoot {
        project_id: "project-golden".to_string(),
        project_root: project_root.to_string_lossy().into_owned(),
    };
    let tauri_fixture = fixture().await;
    tauri_fixture
        .service
        .attach_project(conversation_id, 0, attachment.clone())
        .await
        .unwrap();
    let tauri = crate::commands::conversation_update_execution_target_inner(
        &tauri_fixture.service,
        ID,
        1,
        serde_json::to_value(&target).unwrap(),
    )
    .await;
    let expected_target = normalized_workspace(
        serde_json::to_value(tauri.data.unwrap()).unwrap(),
        &tauri_fixture.workspace_cwd,
    );

    let http_fixture = fixture().await;
    http_fixture
        .service
        .attach_project(conversation_id, 0, attachment.clone())
        .await
        .unwrap();
    let http_workspace_cwd = http_fixture.workspace_cwd.clone();
    let http = response_json(
        app(http_fixture.state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/conversations/{ID}/execution-target"))
                    .body(Body::from(
                        json!({"expectedRevision":1,"executionTarget":target}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        normalized_workspace(http["data"].clone(), &http_workspace_cwd),
        expected_target
    );

    let ws_fixture = fixture().await;
    ws_fixture
        .service
        .attach_project(conversation_id, 0, attachment.clone())
        .await
        .unwrap();
    let ws = dispatch_conversation_golden_request(
        &json!({
            "id":"target-1",
            "type":"update_execution_target",
            "payload":{"conversationId":ID,"expectedRevision":1,"executionTarget":target}
        })
        .to_string(),
        &mut authed,
        &ws_fixture.service,
    )
    .await;
    assert_eq!(
        normalized_workspace(ws.payload.unwrap(), &ws_fixture.workspace_cwd),
        expected_target
    );

    let tauri_fixture = fixture().await;
    tauri_fixture
        .service
        .attach_project(conversation_id, 0, attachment.clone())
        .await
        .unwrap();
    let tauri =
        crate::commands::conversation_detach_project_inner(&tauri_fixture.service, ID, 1).await;
    let expected_detach = normalized_workspace(
        serde_json::to_value(tauri.data.unwrap()).unwrap(),
        &tauri_fixture.workspace_cwd,
    );

    let http_fixture = fixture().await;
    http_fixture
        .service
        .attach_project(conversation_id, 0, attachment.clone())
        .await
        .unwrap();
    let http_workspace_cwd = http_fixture.workspace_cwd.clone();
    let http = response_json(
        app(http_fixture.state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/conversations/{ID}/detach-project"))
                    .body(Body::from(json!({"expectedRevision":1}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        normalized_workspace(http["data"].clone(), &http_workspace_cwd),
        expected_detach
    );

    let ws_fixture = fixture().await;
    ws_fixture
        .service
        .attach_project(conversation_id, 0, attachment)
        .await
        .unwrap();
    let ws = dispatch_conversation_golden_request(
        &json!({
            "id":"detach-1",
            "type":"detach_project",
            "payload":{"conversationId":ID,"expectedRevision":1}
        })
        .to_string(),
        &mut authed,
        &ws_fixture.service,
    )
    .await;
    assert_eq!(
        normalized_workspace(ws.payload.unwrap(), &ws_fixture.workspace_cwd),
        expected_detach
    );
}

#[tokio::test]
async fn lifecycle_transport_golden_matrix_uses_the_same_revisioned_outcome() {
    let conversation_id = ConversationId::parse(ID).unwrap();

    let domain_fixture = fixture_with_lifecycle().await;
    let expected = normalized_workspace(
        serde_json::to_value(
            domain_fixture
                .service
                .detach_binding(conversation_id, 1)
                .await
                .unwrap(),
        )
        .unwrap(),
        &domain_fixture.workspace_cwd,
    );

    let http_fixture = fixture_with_lifecycle().await;
    let http_workspace_cwd = http_fixture.workspace_cwd.clone();
    let response = app(http_fixture.state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/conversations/{ID}/lifecycle/detach"))
                .body(Body::from(json!({"expectedRevision":1}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        normalized_workspace(
            response_json(response).await["data"].clone(),
            &http_workspace_cwd
        ),
        expected
    );

    let ws_fixture = fixture_with_lifecycle().await;
    let mut authed = true;
    let ws = dispatch_conversation_golden_request(
        &json!({
            "id":"detach-golden",
            "type":"detach_binding",
            "payload":{"conversationId":ID,"expectedRevision":1}
        })
        .to_string(),
        &mut authed,
        &ws_fixture.service,
    )
    .await;
    assert_eq!(
        normalized_workspace(ws.payload.unwrap(), &ws_fixture.workspace_cwd),
        expected
    );
}

#[tokio::test]
async fn recovery_action_transport_golden_matrix_preserves_immutable_evidence() {
    let idempotency_key = "21aee10a-56b8-4624-a5e7-586c25dc8d1f";

    let tauri_fixture = fixture().await;
    let tauri_item = seed_recovery(&tauri_fixture.repository);
    let request = json!({
        "recoveryId":tauri_item.recovery_id,
        "expectedRevision":tauri_item.revision,
        "idempotencyKey":idempotency_key,
        "action":"associateConversation",
        "payload":{"conversationId":ID}
    });
    let tauri = crate::commands::conversation_recovery_resolve_inner(
        &tauri_fixture.service,
        request.clone(),
    )
    .await;
    assert!(tauri.success);
    let expected = serde_json::to_value(tauri.data.unwrap()).unwrap();

    let http_fixture = fixture().await;
    let http_item = seed_recovery(&http_fixture.repository);
    assert_eq!(http_item.recovery_id, tauri_item.recovery_id);
    let http = app(http_fixture.state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/conversation-recovery/resolve")
                .body(Body::from(request.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(http.status(), axum::http::StatusCode::OK);
    assert_eq!(response_json(http).await["data"], expected);

    let ws_fixture = fixture().await;
    let ws_item = seed_recovery(&ws_fixture.repository);
    assert_eq!(ws_item.recovery_id, tauri_item.recovery_id);
    let mut authed = true;
    let ws = dispatch_conversation_golden_request(
        &json!({
            "id":"recovery-golden",
            "type":"resolve_recovery_item",
            "payload":request
        })
        .to_string(),
        &mut authed,
        &ws_fixture.service,
    )
    .await;
    assert_eq!(ws.payload.unwrap(), expected);
    assert_eq!(
        expected["sourcePaths"],
        serde_json::to_value(tauri_item.source_paths).unwrap()
    );
    assert_eq!(
        expected["sourceSha256"],
        serde_json::to_value(tauri_item.source_sha256).unwrap()
    );
    assert_eq!(
        expected["provenance"],
        serde_json::to_value(tauri_item.provenance).unwrap()
    );
}

#[tokio::test]
async fn malformed_unauthorized_ping_close_and_duplicate_requests_do_not_double_apply() {
    let fixture = fixture().await;
    let item = seed_recovery(&fixture.repository);
    let request = json!({
        "recoveryId":item.recovery_id,
        "expectedRevision":item.revision,
        "idempotencyKey":"21aee10a-56b8-4624-a5e7-586c25dc8d1f",
        "action":"associateConversation",
        "payload":{"conversationId":ID}
    });

    let first =
        crate::commands::conversation_recovery_resolve_inner(&fixture.service, request.clone())
            .await;
    let duplicate =
        crate::commands::conversation_recovery_resolve_inner(&fixture.service, request.clone())
            .await;
    assert!(first.success && duplicate.success);
    assert_eq!(
        serde_json::to_value(first.data.unwrap()).unwrap(),
        serde_json::to_value(duplicate.data.unwrap()).unwrap()
    );

    let queue = fixture.service.host_status().unwrap();
    assert_eq!(queue.recovery_item_count, 0);

    let mut unauthenticated = false;
    let unauthorized = dispatch_conversation_golden_request(
        r#"{"id":"u-1","type":"list_conversations","payload":{}}"#,
        &mut unauthenticated,
        &fixture.service,
    )
    .await;
    assert_eq!(unauthorized.err.unwrap().code, "UNAUTHORIZED");

    let mut authed = true;
    let malformed =
        dispatch_conversation_golden_request("not-json", &mut authed, &fixture.service).await;
    assert_eq!(malformed.err.unwrap().code, "unsupported");
    let ping = dispatch_conversation_golden_request(
        r#"{"id":"ping-1","type":"ping","payload":{}}"#,
        &mut authed,
        &fixture.service,
    )
    .await;
    assert!(ping.ok);

    let before = fixture
        .repository
        .get_conversation(ConversationId::parse(ID).unwrap())
        .unwrap();
    let ws_source = include_str!("ws.rs");
    assert!(ws_source.contains("Message::Close(_) | Message::Ping(_) | Message::Pong(_)"));
    assert!(ws_source.contains("Axum auto-answers pings; Close ends the loop"));
    let after = fixture
        .repository
        .get_conversation(ConversationId::parse(ID).unwrap())
        .unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn authenticated_remote_mutation_uses_capability_not_proxy_peer_address() {
    let fixture = fixture().await;
    let item = seed_recovery(&fixture.repository);
    let response = app(fixture.state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/conversation-recovery/resolve")
                .extension(ConnectInfo(SocketAddr::from(([192, 168, 1, 5], 3000))))
                .body(Body::from(
                    json!({
                        "recoveryId":item.recovery_id,
                        "expectedRevision":item.revision,
                        "idempotencyKey":"21aee10a-56b8-4624-a5e7-586c25dc8d1f",
                        "action":"associateConversation",
                        "payload":{"conversationId":ID}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    // Since the shared RemoteAccessAuthority replaced peer-IP trust, an authenticated
    // remote principal is authorized independent of proxy address. The mutation applies
    // exactly once through the same golden transport envelope.
    assert_eq!(body["success"], true);
    assert_eq!(
        fixture.service.host_status().unwrap().recovery_item_count,
        0
    );
}
