use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

use codex_extension_api::ConversationHistory;
use codex_extension_api::DetachedReviewThreadContext;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionDataInit;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::NoopTurnItemEmitter;
use codex_extension_api::ThreadStartInput;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolName;
use codex_extension_api::ToolPayload;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_utils_output_truncation::TruncationPolicy;
use factory_extension::FACTORY_STAGE_METADATA_KEY;
use factory_extension::FactoryBackendError;
use factory_extension::FactoryBackendFuture;
use factory_extension::FactoryProgressStatus;
use factory_extension::FactoryReviewVerdict;
use factory_extension::FactoryState;
use factory_extension::FactoryStateBackend;
use factory_extension::FactoryStateDurability;
use factory_extension::FactoryTurnStage;
use factory_extension::FactoryWorkUnit;
use factory_extension::install_with_backend;
use factory_extension::thread_state;
use serde_json::json;

#[derive(Default)]
struct TestStateBackend {
    states: RwLock<HashMap<String, FactoryState>>,
}

impl FactoryStateBackend for TestStateBackend {
    fn load<'a>(&'a self, thread_id: &'a str) -> FactoryBackendFuture<'a, Option<FactoryState>> {
        Box::pin(async move {
            self.states
                .read()
                .map(|states| states.get(thread_id).cloned())
                .map_err(|_| FactoryBackendError::new("test state backend lock failed"))
        })
    }

    fn save<'a>(&'a self, thread_id: &'a str, state: FactoryState) -> FactoryBackendFuture<'a, ()> {
        Box::pin(async move {
            self.states
                .write()
                .map_err(|_| FactoryBackendError::new("test state backend lock failed"))?
                .insert(thread_id.to_string(), state);
            Ok(())
        })
    }

    fn durability(&self) -> FactoryStateDurability {
        FactoryStateDurability::ProcessMemory
    }
}

#[tokio::test]
async fn detached_review_shares_parent_state_and_records_exact_lineage() {
    let mut builder = ExtensionRegistryBuilder::<()>::new();
    install_with_backend(
        &mut builder,
        Arc::new(TestStateBackend::default()),
        FactoryTurnStage::Plan,
    );
    let registry = builder.build();

    let config = ();
    let session_store = ExtensionData::new("factory-session");
    let parent_thread_id = "11111111-1111-4111-8111-111111111111";
    let parent_store = ExtensionData::new(parent_thread_id);
    let parent_source = SessionSource::Cli;
    start_thread(
        &registry,
        &config,
        &parent_source,
        &session_store,
        &parent_store,
    )
    .await;

    let parent_tools = registry.tool_contributors()[0].tools(&session_store, &parent_store);
    find_tool(&parent_tools, "factory_decompose")
        .handle(tool_call(
            "factory_decompose",
            "parent-turn",
            json!({
                "units": [{
                    "id": "audit",
                    "title": "Audit",
                    "description": "Audit the repository",
                    "depends_on": []
                }]
            }),
        ))
        .await
        .expect("parent decomposition should be recorded");

    let review_thread_id = "22222222-2222-4222-8222-222222222222";
    let parent_turn_id = "execute-turn-9";
    let mut review_init = ExtensionDataInit::new();
    review_init.insert(DetachedReviewThreadContext {
        parent_thread_id: ThreadId::from_string(parent_thread_id).expect("valid parent thread id"),
        parent_turn_id: parent_turn_id.to_string(),
        durable_state_key: parent_thread_id.to_string(),
    });
    let review_store = ExtensionData::new_with_init(review_thread_id, review_init);
    let review_source = SessionSource::SubAgent(SubAgentSource::Review);
    start_thread(
        &registry,
        &config,
        &review_source,
        &session_store,
        &review_store,
    )
    .await;

    let parent_state = thread_state(&parent_store).expect("parent Factory state");
    let review_state = thread_state(&review_store).expect("review Factory state");
    assert!(Arc::ptr_eq(&parent_state, &review_state));
    assert_eq!(review_state.thread_id(), parent_thread_id);
    assert_eq!(
        review_state
            .snapshot()
            .await
            .expect("shared state")
            .work_units[0]
            .status,
        FactoryProgressStatus::Pending
    );

    let review_tools = registry.tool_contributors()[0].tools(&session_store, &review_store);
    find_tool(&review_tools, "factory_record_review")
        .handle(tool_call(
            "factory_record_review",
            "review-turn-7",
            json!({
                "verdict": "approve",
                "summary": "No findings",
                "findings": []
            }),
        ))
        .await
        .expect("detached review should be recorded");

    let report = parent_state
        .snapshot()
        .await
        .expect("parent state after review")
        .review
        .expect("review report");
    assert_eq!(report.verdict, FactoryReviewVerdict::Approve);
    assert_eq!(report.recorded_thread_id.as_deref(), Some(review_thread_id));
    assert_eq!(report.recorded_turn_id.as_deref(), Some("review-turn-7"));
    assert_eq!(
        report.recorded_parent_thread_id.as_deref(),
        Some(parent_thread_id)
    );
    assert_eq!(
        report.recorded_parent_turn_id.as_deref(),
        Some(parent_turn_id)
    );
    assert_eq!(report.recorded_subagent_kind.as_deref(), Some("review"));
}

#[tokio::test]
async fn review_tool_enforces_verdict_findings_contract_before_state_mutation() {
    let backend = Arc::new(TestStateBackend::default());
    let thread_id = "11111111-1111-4111-8111-111111111111";
    backend.states.write().expect("backend state lock").insert(
        thread_id.to_string(),
        FactoryState {
            work_units: vec![FactoryWorkUnit {
                id: "proof".to_string(),
                title: "Prove output".to_string(),
                description: "Verify the deterministic output".to_string(),
                depends_on: Vec::new(),
                status: FactoryProgressStatus::Completed,
                progress_summary: Some("Output verified before review.".to_string()),
            }],
            ..FactoryState::default()
        },
    );
    let mut builder = ExtensionRegistryBuilder::<()>::new();
    install_with_backend(&mut builder, backend.clone(), FactoryTurnStage::Review);
    let registry = builder.build();

    let config = ();
    let session_store = ExtensionData::new("factory-session");
    let thread_store = ExtensionData::new(thread_id);
    let source = SessionSource::Cli;
    start_thread(&registry, &config, &source, &session_store, &thread_store).await;

    let tools = registry.tool_contributors()[0].tools(&session_store, &thread_store);
    let state = thread_state(&thread_store).expect("Factory state");
    let before_invalid_approve = state
        .snapshot()
        .await
        .expect("state before invalid approve");
    let review_tool = find_tool(&tools, "factory_record_review");
    let invalid_approve = review_tool
        .handle(tool_call(
            "factory_record_review",
            "review-turn-invalid-approve",
            json!({
                "verdict": "approve",
                "summary": "The output is correct.",
                "findings": [{
                    "id": "positive-note",
                    "severity": "minor",
                    "unit_id": "proof",
                    "title": "Output matches",
                    "evidence": "The bytes are identical.",
                    "recommendation": "No change required."
                }]
            }),
        ))
        .await;
    assert!(invalid_approve.is_err());
    assert_eq!(
        state.snapshot().await.expect("state after invalid approve"),
        before_invalid_approve,
        "an invalid approve call must not mutate persisted Factory state"
    );

    let before_invalid_changes = state
        .snapshot()
        .await
        .expect("state before invalid request changes");
    let invalid_changes = review_tool
        .handle(tool_call(
            "factory_record_review",
            "review-turn-invalid-changes",
            json!({
                "verdict": "request_changes",
                "summary": "Changes are needed.",
                "findings": []
            }),
        ))
        .await;
    assert!(invalid_changes.is_err());
    assert_eq!(
        state
            .snapshot()
            .await
            .expect("state after invalid request changes"),
        before_invalid_changes,
        "request_changes without findings must not mutate persisted Factory state"
    );

    review_tool
        .handle(tool_call(
            "factory_record_review",
            "review-turn-valid-approve",
            json!({
                "verdict": "approve",
                "summary": "The exact bytes and verifier result pass.",
                "findings": []
            }),
        ))
        .await
        .expect("approve with an empty findings array should be recorded");

    let approved = state.snapshot().await.expect("state after valid approve");
    let report = approved.review.as_ref().expect("approved report");
    assert_eq!(report.verdict, FactoryReviewVerdict::Approve);
    assert!(report.findings.is_empty());
    assert_eq!(approved.revision, before_invalid_approve.revision + 1);
    assert_eq!(
        backend
            .states
            .read()
            .expect("backend state lock")
            .get(state.thread_id()),
        Some(&approved),
        "the valid review must be persisted by the configured state backend"
    );
}

#[tokio::test]
async fn mutating_tools_enforce_stage_and_single_plan_decomposition() {
    let mut builder = ExtensionRegistryBuilder::<()>::new();
    install_with_backend(
        &mut builder,
        Arc::new(TestStateBackend::default()),
        FactoryTurnStage::Plan,
    );
    let registry = builder.build();

    let config = ();
    let session_store = ExtensionData::new("factory-session");
    let thread_store = ExtensionData::new("11111111-1111-4111-8111-111111111111");
    start_thread(
        &registry,
        &config,
        &SessionSource::Cli,
        &session_store,
        &thread_store,
    )
    .await;
    let tools = registry.tool_contributors()[0].tools(&session_store, &thread_store);
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.tool_name().to_string())
            .collect::<Vec<_>>(),
        vec!["factory_decompose"]
    );
    let state = thread_state(&thread_store).expect("Factory state");
    let decomposition = json!({
        "units": [{
            "id": "proof",
            "title": "Prove output",
            "description": "Verify the deterministic output",
            "depends_on": []
        }]
    });

    let wrong_stage = find_tool(&tools, "factory_decompose")
        .handle(tool_call_at_stage(
            "factory_decompose",
            "execute-turn",
            decomposition.clone(),
            Some("codex.execute"),
        ))
        .await;
    assert!(wrong_stage.is_err());
    assert_eq!(
        state.snapshot().await.expect("state after denied call"),
        FactoryState::default()
    );

    find_tool(&tools, "factory_decompose")
        .handle(tool_call(
            "factory_decompose",
            "plan-turn",
            decomposition.clone(),
        ))
        .await
        .expect("Plan decomposition should be accepted once");
    let planned = state.snapshot().await.expect("planned state");

    let repeated = find_tool(&tools, "factory_decompose")
        .handle(tool_call("factory_decompose", "plan-turn", decomposition))
        .await;
    assert!(repeated.is_err());
    assert_eq!(state.snapshot().await.expect("state after repeat"), planned);
}

#[tokio::test]
async fn ordinary_subagents_do_not_receive_factory_mutation_tools() {
    let mut builder = ExtensionRegistryBuilder::<()>::new();
    install_with_backend(
        &mut builder,
        Arc::new(TestStateBackend::default()),
        FactoryTurnStage::Execute,
    );
    let registry = builder.build();
    let session_store = ExtensionData::new("factory-session");
    let thread_store = ExtensionData::new("33333333-3333-4333-8333-333333333333");
    start_thread(
        &registry,
        &(),
        &SessionSource::SubAgent(SubAgentSource::Compact),
        &session_store,
        &thread_store,
    )
    .await;

    assert!(
        registry.tool_contributors()[0]
            .tools(&session_store, &thread_store)
            .is_empty()
    );
}

async fn start_thread(
    registry: &codex_extension_api::ExtensionRegistry<()>,
    config: &(),
    source: &SessionSource,
    session_store: &ExtensionData,
    thread_store: &ExtensionData,
) {
    for contributor in registry.thread_lifecycle_contributors() {
        contributor
            .on_thread_start(ThreadStartInput {
                config,
                session_source: source,
                persistent_thread_state_available: true,
                environments: &[],
                mcp_resource_client: None,
                extension_metrics: None,
                session_store,
                thread_store,
            })
            .await;
    }
}

fn find_tool<'a>(
    tools: &'a [Arc<dyn codex_extension_api::ToolExecutor<ToolCall>>],
    name: &str,
) -> &'a Arc<dyn codex_extension_api::ToolExecutor<ToolCall>> {
    tools
        .iter()
        .find(|tool| tool.tool_name() == ToolName::plain(name))
        .expect("Factory tool should be installed")
}

fn tool_call(name: &str, turn_id: &str, arguments: serde_json::Value) -> ToolCall {
    let stage = match name {
        "factory_decompose" => Some("codex.plan"),
        "factory_update_progress" => Some("codex.execute"),
        "factory_record_review" => Some("codex.review"),
        "factory_record_remediation" => Some("codex.remediate"),
        _ => None,
    };
    tool_call_at_stage(name, turn_id, arguments, stage)
}

fn tool_call_at_stage(
    name: &str,
    turn_id: &str,
    arguments: serde_json::Value,
    stage: Option<&str>,
) -> ToolCall {
    ToolCall {
        turn_id: turn_id.to_string(),
        call_id: format!("{turn_id}-call"),
        tool_name: ToolName::plain(name),
        model: "mock-model".to_string(),
        codex_turn_metadata: stage
            .map(|stage| json!({FACTORY_STAGE_METADATA_KEY: stage}).to_string()),
        truncation_policy: TruncationPolicy::Bytes(1024),
        conversation_history: ConversationHistory::default(),
        turn_item_emitter: Arc::new(NoopTurnItemEmitter),
        environments: Vec::new(),
        payload: ToolPayload::Function {
            arguments: arguments.to_string(),
        },
    }
}
