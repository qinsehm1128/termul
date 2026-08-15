use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::json;
use uuid::Uuid;

use super::migration::MigrationMapV1;
use super::*;

const ID: &str = "018f7a1c-1b4d-7c8a-9f01-0123456789ab";

type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Default)]
struct MatrixProvider {
    addressable: Mutex<bool>,
    suspended: Mutex<Vec<String>>,
    replacements: Mutex<Vec<String>>,
}

impl ConversationAgentLifecycle for MatrixProvider {
    fn owns_session<'a>(&'a self, _binding: &'a AgentSessionBinding) -> ProviderFuture<'a, bool> {
        Box::pin(async move { *self.addressable.lock() })
    }

    fn suspend<'a>(
        &'a self,
        binding: &'a AgentSessionBinding,
    ) -> ProviderFuture<'a, std::result::Result<(), AgentLifecycleProviderError>> {
        Box::pin(async move {
            self.suspended.lock().push(binding.agent_session_id.clone());
            Ok(())
        })
    }

    fn replace<'a>(
        &'a self,
        previous_binding: &'a AgentSessionBinding,
        _prepared: &'a PreparedConversation,
    ) -> ProviderFuture<'a, std::result::Result<AgentBindingResult, AgentLifecycleProviderError>>
    {
        Box::pin(async move {
            self.replacements
                .lock()
                .push(previous_binding.agent_session_id.clone());
            Ok(AgentBindingResult {
                agent_session_id: "opaque/replacement".to_string(),
                runtime_agent_id: "runtime-replacement".to_string(),
                stable_agent_namespace: "config:replacement".to_string(),
            })
        })
    }

    fn abort_replacement<'a>(
        &'a self,
        _binding: &'a AgentSessionBinding,
    ) -> ProviderFuture<'a, ()> {
        Box::pin(async {})
    }

    fn register_binding(&self, _agent_session_id: &str, _conversation_id: ConversationId) {}
}

#[derive(Default)]
struct MatrixTerminals(Mutex<HashSet<String>>);

impl TerminalResourceInspector for MatrixTerminals {
    fn is_live(&self, terminal_id: &str) -> bool {
        self.0.lock().contains(terminal_id)
    }
}

struct MatrixFixture {
    _temp: tempfile::TempDir,
    repository: Arc<ConversationRepository>,
    workspace: Arc<SessionWorkspaceService>,
    application: Arc<ConversationApplicationService>,
    provider: Arc<MatrixProvider>,
    terminals: Arc<MatrixTerminals>,
    id: ConversationId,
}

fn fixed_time() -> DateTime<Utc> {
    parse_created_at_utc("2026-08-15T09:45:15.123Z").unwrap()
}

async fn fixture() -> MatrixFixture {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let private = base.join("state/conversations/v2");
    let visible = base.join("visible/sessions/2026/08/15").join(ID);
    std::fs::create_dir_all(&visible).unwrap();
    let (repository, report) = ConversationRepository::open(private.clone()).unwrap();
    assert_eq!(report.valid_conversation_count, 0);
    let id = ConversationId::parse(ID).unwrap();
    let created_at = fixed_time();
    repository
        .create_conversation(ConversationRecordV2 {
            schema_version: CONVERSATION_SCHEMA_VERSION,
            conversation_id: id,
            created_at_utc: created_at,
            creation_partition: CreationPartition::from_created_at(created_at),
            workspace_cwd: visible.to_string_lossy().into_owned(),
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
                agent_session_id: "opaque/original".to_string(),
                runtime_agent_id: "runtime-original".to_string(),
                stable_agent_namespace: "config:original".to_string(),
                execution_cwd: visible.to_string_lossy().into_owned(),
                bound_at_utc: created_at,
                state: AgentSessionBindingState::Active,
            },
            created_at,
        )
        .await
        .unwrap();

    let creation = Arc::new(
        ConversationCreationService::new(
            Arc::clone(&repository),
            ConversationLocator::new(private).unwrap(),
            SessionWorkspaceLocator::new(base.join("visible")).unwrap(),
        )
        .unwrap(),
    );
    let reader = Arc::new(ConversationReader::new(
        Arc::clone(&repository),
        LegacyConversationReader::default(),
        ReaderPrecedence::ConversationV2Only,
    ));
    let workspace = Arc::new(SessionWorkspaceService::new(Arc::clone(&repository)));
    let application = Arc::new(ConversationApplicationService::new(
        reader,
        Arc::clone(&workspace),
        &MigrationMapV1 {
            schema_version: migration::MIGRATION_MAP_SCHEMA_VERSION,
            operation_id: Uuid::new_v4(),
            entries: Vec::new(),
        },
        MigrationHostMode::Standalone,
        MigrationPhase::Finalized,
        ReaderPrecedence::ConversationV2Only,
        0,
    ));
    let provider = Arc::new(MatrixProvider::default());
    *provider.addressable.lock() = true;
    let terminals = Arc::new(MatrixTerminals::default());
    application
        .attach_lifecycle(ConversationLifecycleService::new(
            Arc::clone(&repository),
            creation,
            provider.clone(),
            terminals.clone(),
        ))
        .unwrap();

    MatrixFixture {
        _temp: temp,
        repository,
        workspace,
        application,
        provider,
        terminals,
        id,
    }
}

fn revision(fixture: &MatrixFixture) -> u64 {
    fixture
        .repository
        .get_conversation(fixture.id)
        .unwrap()
        .last_seq
}

#[tokio::test]
async fn lifecycle_matrix() {
    let fixture = fixture().await;
    let identity = fixture.repository.get_conversation(fixture.id).unwrap();
    let cases = [
        ("detach", AgentSessionBindingState::Detached),
        ("rebind", AgentSessionBindingState::Active),
        ("suspend", AgentSessionBindingState::Suspended),
        ("replace", AgentSessionBindingState::Active),
    ];

    for (operation, expected_state) in cases {
        let expected_revision = revision(&fixture);
        let outcome = match operation {
            "detach" => {
                fixture
                    .application
                    .detach_binding(fixture.id, expected_revision)
                    .await
            }
            "rebind" => {
                fixture
                    .application
                    .rebind_binding(fixture.id, expected_revision)
                    .await
            }
            "suspend" => {
                fixture
                    .application
                    .suspend_binding(fixture.id, expected_revision)
                    .await
            }
            "replace" => {
                fixture
                    .application
                    .replace_binding(
                        fixture.id,
                        PrepareConversationRequest {
                            schema_version: 1,
                            conversation_id: Some(fixture.id),
                            project_attachment: None,
                            execution_target: ExecutionTarget::Workspace,
                        },
                        expected_revision,
                    )
                    .await
            }
            _ => unreachable!(),
        }
        .unwrap();
        let ConversationLifecycleOutcome::Updated {
            conversation_id,
            revision: outcome_revision,
            current_binding,
            ..
        } = outcome
        else {
            panic!("{operation} must update")
        };
        assert_eq!(conversation_id, fixture.id, "operation={operation}");
        assert!(
            outcome_revision > expected_revision,
            "operation={operation}"
        );
        assert_eq!(
            current_binding.unwrap().state,
            expected_state,
            "operation={operation}"
        );
        let current = fixture.repository.get_conversation(fixture.id).unwrap();
        assert_eq!(current.conversation_id, identity.conversation_id);
        assert_eq!(current.created_at_utc, identity.created_at_utc);
        assert_eq!(current.creation_partition, identity.creation_partition);
        assert_eq!(current.workspace_cwd, identity.workspace_cwd);
    }

    assert_eq!(
        fixture.provider.suspended.lock().as_slice(),
        ["opaque/original"]
    );
    assert_eq!(
        fixture.provider.replacements.lock().as_slice(),
        ["opaque/original"]
    );

    fixture
        .workspace
        .add_terminal_ref(fixture.id, "terminal-live")
        .await
        .unwrap();
    fixture
        .terminals
        .0
        .lock()
        .insert("terminal-live".to_string());
    let blocked = fixture
        .application
        .delete_conversation(fixture.id, revision(&fixture))
        .await
        .unwrap();
    assert!(matches!(
        blocked,
        ConversationLifecycleOutcome::Blocked { blockers, .. }
            if blockers.iter().any(|blocker| matches!(blocker, ConversationDeleteBlocker::LiveBinding { .. }))
                && blockers.iter().any(|blocker| matches!(blocker, ConversationDeleteBlocker::TerminalResources { .. }))
    ));
    assert!(fixture.terminals.is_live("terminal-live"));

    let events = fixture.repository.read_events(fixture.id, 0).unwrap();
    for expected in [
        ConversationEventType::BindingDetached,
        ConversationEventType::BindingRebound,
        ConversationEventType::BindingSuspended,
        ConversationEventType::BindingReplaced,
    ] {
        assert!(events.iter().any(|event| event.type_ == expected));
    }
}

#[tokio::test]
async fn repository_catalog_workspace_and_application_recovery_matrix() {
    let fixture = fixture().await;
    let catalog_path = fixture.repository.root().join("catalog.json");
    let canonical_catalog = std::fs::read(&catalog_path).unwrap();
    std::fs::write(&catalog_path, b"not-json").unwrap();
    let (reopened, report) =
        ConversationRepository::open(fixture.repository.root().to_path_buf()).unwrap();
    assert_eq!(std::fs::read(&catalog_path).unwrap(), canonical_catalog);
    assert_eq!(reopened.list_conversations().len(), 1);
    assert!(report
        .recovery_items
        .iter()
        .any(|item| item.kind == RepositoryRecoveryKind::CatalogIgnored));

    let open = fixture
        .application
        .open_conversation(fixture.id)
        .await
        .unwrap();
    assert_eq!(open.conversation.conversation_id, fixture.id);
    assert!(matches!(
        open.workspace,
        SessionWorkspaceLoadOutcome::Missing { .. }
    ));

    let write = fixture
        .application
        .write_workspace(
            fixture.id,
            None,
            SessionWorkspaceV1 {
                schema_version: SESSION_WORKSPACE_SCHEMA_VERSION,
                conversation_id: fixture.id,
                revision: 0,
                updated_at_utc: String::new(),
                update_identity: Some("validation-matrix".to_string()),
                topology: None,
                active_pane_id: None,
                resources: Vec::new(),
                projection_state: SessionWorkspaceProjectionState::Native,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        write,
        SessionWorkspaceWriteOutcome::Updated { revision: 1, .. }
    ));
    let conflict = fixture
        .application
        .write_workspace(
            fixture.id,
            None,
            SessionWorkspaceV1 {
                schema_version: SESSION_WORKSPACE_SCHEMA_VERSION,
                conversation_id: fixture.id,
                revision: 0,
                updated_at_utc: String::new(),
                update_identity: Some("stale".to_string()),
                topology: None,
                active_pane_id: None,
                resources: Vec::new(),
                projection_state: SessionWorkspaceProjectionState::Native,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        conflict,
        SessionWorkspaceWriteOutcome::Conflict {
            current_revision: 1,
            ..
        }
    ));

    let workspace_path = fixture.repository.workspace_path(fixture.id).unwrap();
    std::fs::write(&workspace_path, b"{corrupt").unwrap();
    let preserved = std::fs::read(&workspace_path).unwrap();
    assert!(matches!(
        fixture.application.get_workspace(fixture.id).await.unwrap(),
        SessionWorkspaceLoadOutcome::RecoveryRequired { .. }
    ));
    assert_eq!(std::fs::read(workspace_path).unwrap(), preserved);
}

#[test]
fn migration_phase_and_shutdown_boundary_matrix() {
    use MigrationPhase::*;
    let transitions = [
        (Detected, Quiescing),
        (Quiescing, Inventoried),
        (Inventoried, Staging),
        (Staging, Verifying),
        (Verifying, CutoverPending),
        (CutoverPending, Committed),
        (Committed, ObservationWindow),
    ];
    let mut journal = MigrationJournalV1::new("a".repeat(64), fixed_time());
    for (current, next) in transitions {
        assert_eq!(journal.phase, current);
        advance_phase(&mut journal, next, fixed_time(), false).unwrap();
    }

    let terminal_ws = include_str!("../web/terminal_ws.rs");
    let disconnect = terminal_ws
        .split("async fn run")
        .nth(1)
        .and_then(|tail| tail.split("struct ConnectionContext").next())
        .unwrap();
    for forbidden in [".terminate(", ".kill(", "kill_all"] {
        assert!(
            !disconnect.contains(forbidden),
            "disconnect contains {forbidden}"
        );
    }
    let remote = include_str!("../remote/host.rs");
    let production = remote.split("#[cfg(test)]").next().unwrap();
    assert!(production.contains("serve_router("));
    assert!(!production.contains("kill_all_checked"));

    let evidence = json!({
        "conversationId": ID,
        "operationId": journal.operation_id,
        "phase": journal.phase,
        "workspaceRevision": 1,
        "terminalId": "terminal-live",
        "transport": "matrix"
    });
    assert_eq!(evidence["phase"], "observation_window");
}
