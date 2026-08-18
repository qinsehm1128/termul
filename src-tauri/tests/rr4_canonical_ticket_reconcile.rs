//! Lifecycle-vs-relay sequence race: published seq is the lock-allocated ticket.

mod rr4_canonical_ticket_reconcile {
    use std::sync::Arc;

    use serde_json::json;
    use termul_manager_lib::conversation::{
        AgentBindingResult, ConversationBootstrap, ExecutionTarget, HostConversationRoots,
        MigrationHostMode, PrepareConversationRequest, PREPARE_CONVERSATION_SCHEMA_VERSION,
    };
    use termul_manager_lib::web::WsRelaySink;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lifecycle_append_race_commits_allocated_seq_without_poisoning_relay() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("state");
        let workspace = temp.path().join("visible");
        let bootstrap = ConversationBootstrap::run(
            HostConversationRoots::desktop(state_root, workspace),
            MigrationHostMode::Desktop,
        )
        .expect("bootstrap");
        let prepared = bootstrap
            .creation
            .create_with_agent_gate(
                PrepareConversationRequest {
                    schema_version: PREPARE_CONVERSATION_SCHEMA_VERSION,
                    conversation_id: None,
                    project_attachment: None,
                    execution_target: ExecutionTarget::Workspace,
                },
                |_prepared| async move {
                    Ok(AgentBindingResult {
                        agent_session_id: "opaque/race".to_string(),
                        runtime_agent_id: "runtime-test".to_string(),
                        stable_agent_namespace: "stable-test".to_string(),
                    })
                },
            )
            .await
            .expect("create conversation and bind agent session");
        let record = bootstrap
            .repository
            .get_conversation(prepared.conversation_id)
            .expect("created conversation");
        assert!(
            record.last_seq >= 1,
            "binding/lifecycle append must consume a canonical seq"
        );

        let relay = Arc::new(WsRelaySink::with_conversation_persistence(
            16,
            Arc::clone(&bootstrap.persistence_adapter),
            None,
        ));
        let published = relay
            .persist_user_prompt("opaque/race", json!({"text": "hello"}))
            .await
            .expect("relay must consume the lock-allocated ticket");
        assert_eq!(
            published.seq,
            record.last_seq + 1,
            "published SequencedEvent.seq equals the lock-allocated ticket"
        );
        let health = relay
            .ordered_conversation_persistence()
            .expect("ordered persistence")
            .health("opaque/race")
            .expect("health");
        assert_eq!(
            health.and_then(|snapshot| snapshot.last_error_code),
            None,
            "CONVERSATION_CONFLICT must not latch last_error_code"
        );
        assert_eq!(
            relay.auxiliary_stats().delivery_circuits,
            0,
            "no 900s ACP Fatal circuit"
        );
        relay
            .shutdown_conversation_persistence()
            .await
            .expect("ordered shutdown");
    }
}
