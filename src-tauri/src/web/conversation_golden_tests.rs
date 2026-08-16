use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::routing::{get, post};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use super::conversation_api;
use super::ws::{dispatch_conversation_golden_request, AppState, HistoryMode};
use crate::conversation::contracts::{
    parse_created_at_utc, ConversationCreator, ConversationLifecycleState, ConversationRecordV2,
    CreationPartition, ExecutionTarget, CONVERSATION_SCHEMA_VERSION,
};
use crate::conversation::migration::{
    CreatedAtSource, IdentityDecision, MigrationHostMode, MigrationMapEntryV1, MigrationMapV1,
    MigrationPhase, ReaderPrecedence, RecoveryItemV1, RecoveryKind, RecoveryProvenanceV1,
    RecoveryQueueV1, RecoverySeverity, MIGRATION_MAP_SCHEMA_VERSION,
};
use crate::conversation::{
    ConversationApplicationService, ConversationId, ConversationMutation, ConversationReader,
    ConversationRepository, ConversationWriter, LegacyConversationReader, SessionWorkspaceService,
};

const ID: &str = "018f7a1c-1b4d-7c8a-9f01-0123456789ab";

struct GoldenFixture {
    _temp: tempfile::TempDir,
    repository: Arc<ConversationRepository>,
    service: Arc<ConversationApplicationService>,
    state: AppState,
}

async fn fixture() -> GoldenFixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp
        .path()
        .canonicalize()
        .unwrap()
        .join("state/conversations/v2");
    let (repository, _) = ConversationRepository::open(root).unwrap();
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
                workspace_cwd: "/visible/golden".to_string(),
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
        writer,
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
        repository,
        service,
        state,
    }
}

fn app(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/conversations", get(conversation_api::list))
        .route(
            "/conversations/resolve-legacy",
            post(conversation_api::resolve_legacy),
        )
        .route(
            "/conversation-recovery/resolve",
            post(conversation_api::resolve_recovery),
        )
        .with_state(state)
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
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
async fn http_remote_mutation_is_forbidden_without_mutating_recovery_state() {
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
    assert_eq!(body["code"], "FORBIDDEN");
    assert_eq!(
        fixture.service.host_status().unwrap().recovery_item_count,
        1
    );
}
