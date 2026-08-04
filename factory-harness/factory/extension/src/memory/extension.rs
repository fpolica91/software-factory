use std::collections::HashSet;
use std::sync::Arc;

use codex_extension_api::ContextualUserFragment;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionMetrics;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::FunctionCallError;
use codex_extension_api::JsonToolOutput;
use codex_extension_api::ResponsesApiTool;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolContributor;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolExecutorFuture;
use codex_extension_api::ToolName;
use codex_extension_api::ToolOutput;
use codex_extension_api::ToolSpec;
use codex_extension_api::TurnInputContext;
use codex_extension_api::TurnInputContributor;
use codex_extension_api::parse_tool_input_schema;
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

use crate::memory::FactoryMemory;
use crate::memory::FactoryMemoryHit;
use crate::memory::FactoryMemoryScope;
use crate::memory::FactoryRepositoryId;
use crate::memory::RepositoryScopedMemory;
use crate::stage::FactoryThreadScope;
use crate::stage::FactoryTurnStage;
use crate::stage::require_tool_stage;
use crate::stage::thread_scope;

const REMEMBER_TOOL: &str = "factory_remember";
const RECALL_TOOL: &str = "factory_recall";
const AUTO_RECALL_LIMIT: usize = 5;
const MAX_RECALL_LIMIT: usize = 8;
const MAX_MEMORY_CONTENT_BYTES: usize = 4096;
const MAX_MEMORY_CONTEXT_BYTES: usize = 12_000;

#[derive(Clone)]
struct FactoryMemoryExtension {
    memory: RepositoryScopedMemory,
    stage: FactoryTurnStage,
}

#[derive(Clone, Copy)]
enum FactoryMemoryToolKind {
    Remember,
    Recall,
}

#[derive(Clone)]
struct FactoryMemoryToolExecutor {
    kind: FactoryMemoryToolKind,
    memory: RepositoryScopedMemory,
    source_thread_id: Arc<str>,
}

#[derive(Debug, Deserialize)]
struct RememberArgs {
    content: String,
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RecallArgs {
    query: String,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct RememberReceipt {
    id: String,
    namespace: String,
    repository_id: String,
    stored: bool,
    tag_count: usize,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct RecallReceipt {
    namespace: String,
    repository_id: String,
    count: usize,
    memories: Vec<FactoryMemoryHit>,
}

#[derive(Debug, Serialize)]
struct FactoryMemoryContext<'a> {
    source: &'static str,
    namespace: &'a str,
    repository_id: &'a str,
    query: &'a str,
    memories: &'a [FactoryMemoryHit],
}

#[derive(Debug)]
struct FactoryMemoryContextFragment {
    body: String,
}

#[derive(Debug)]
struct FactoryMemoryErrorFragment {
    body: String,
}

pub(crate) fn install_memory<C: Sync + 'static>(
    registry: &mut ExtensionRegistryBuilder<C>,
    memory: FactoryMemory,
    repository_id: FactoryRepositoryId,
    stage: FactoryTurnStage,
) {
    let extension = Arc::new(FactoryMemoryExtension {
        memory: memory.for_repository(repository_id),
        stage,
    });
    registry.tool_contributor(extension.clone());
    registry.turn_input_contributor(extension);
}

impl ToolContributor for FactoryMemoryExtension {
    fn tools(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
    ) -> Vec<Arc<dyn ToolExecutor<ToolCall>>> {
        let source_thread_id: Arc<str> = thread_store.level_id().to_string().into();
        let stage = match thread_scope(thread_store) {
            FactoryThreadScope::Parent => self.stage,
            FactoryThreadScope::DetachedReview | FactoryThreadScope::Subagent => {
                FactoryTurnStage::Review
            }
        };
        stage
            .memory_tool_kinds()
            .iter()
            .copied()
            .map(|kind| {
                Arc::new(FactoryMemoryToolExecutor {
                    kind,
                    memory: self.memory.clone(),
                    source_thread_id: Arc::clone(&source_thread_id),
                }) as Arc<dyn ToolExecutor<ToolCall>>
            })
            .collect()
    }
}

impl ToolExecutor<ToolCall> for FactoryMemoryToolExecutor {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(self.kind.name())
    }

    fn spec(&self) -> ToolSpec {
        self.kind.spec()
    }

    fn handle(&self, call: ToolCall) -> ToolExecutorFuture<'_> {
        Box::pin(async move {
            match self.kind {
                FactoryMemoryToolKind::Remember => {
                    require_tool_stage(
                        &call,
                        REMEMBER_TOOL,
                        &[FactoryTurnStage::Execute, FactoryTurnStage::Remediate],
                    )
                    .map_err(respond)?;
                    self.remember(call).await
                }
                FactoryMemoryToolKind::Recall => self.recall(call).await,
            }
        })
    }
}

impl FactoryMemoryToolExecutor {
    async fn remember(&self, call: ToolCall) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let mut args: RememberArgs = parse_args(&call)?;
        require_text("memory content", &args.content).map_err(respond)?;
        if args.content.len() > MAX_MEMORY_CONTENT_BYTES {
            return Err(respond(format!(
                "memory content exceeds {MAX_MEMORY_CONTENT_BYTES} bytes"
            )));
        }
        validate_tags(&mut args.tags).map_err(respond)?;
        let memory = self
            .memory
            .remember(&self.source_thread_id, args.content, args.tags)
            .await
            .map_err(|error| respond(error.to_string()))?;
        json_output(RememberReceipt {
            id: memory.id,
            namespace: memory.namespace,
            repository_id: memory.repository_id,
            stored: true,
            tag_count: memory.tags.len(),
            created_at: memory.created_at,
        })
    }

    async fn recall(&self, call: ToolCall) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let args: RecallArgs = parse_args(&call)?;
        require_text("memory query", &args.query).map_err(respond)?;
        let limit = args.limit.unwrap_or(AUTO_RECALL_LIMIT);
        if !(1..=MAX_RECALL_LIMIT).contains(&limit) {
            return Err(respond(format!(
                "memory recall limit must be between 1 and {MAX_RECALL_LIMIT}"
            )));
        }
        let memories = self
            .memory
            .recall(&args.query, limit)
            .await
            .map_err(|error| respond(error.to_string()))?;
        let scope = self.memory.scope();
        json_output(RecallReceipt {
            namespace: scope.namespace().to_string(),
            repository_id: scope.repository_id().to_string(),
            count: memories.len(),
            memories,
        })
    }
}

impl FactoryMemoryToolKind {
    fn name(self) -> &'static str {
        match self {
            Self::Remember => REMEMBER_TOOL,
            Self::Recall => RECALL_TOOL,
        }
    }

    fn spec(self) -> ToolSpec {
        let (description, schema) = match self {
            Self::Remember => (
                "During codex.execute or codex.remediate only, persist one durable Factory memory for later threads in this repository. Store only information worth recalling beyond this turn.",
                remember_schema(),
            ),
            Self::Recall => (
                "Search durable Factory memory for this repository and return ranked records with their source thread and tags.",
                recall_schema(),
            ),
        };
        ToolSpec::Function(ResponsesApiTool {
            name: self.name().to_string(),
            description: description.to_string(),
            strict: false,
            defer_loading: None,
            parameters: parse_tool_input_schema(&schema)
                .expect("Factory-owned memory tool schema must be valid"),
            output_schema: None,
        })
    }
}

impl FactoryTurnStage {
    fn memory_tool_kinds(self) -> &'static [FactoryMemoryToolKind] {
        match self {
            Self::Plan | Self::Review => &[FactoryMemoryToolKind::Recall],
            Self::Execute | Self::Remediate => &[
                FactoryMemoryToolKind::Remember,
                FactoryMemoryToolKind::Recall,
            ],
        }
    }
}

impl TurnInputContributor for FactoryMemoryExtension {
    fn contribute<'a>(
        &'a self,
        input: TurnInputContext,
        _extension_metrics: Option<Arc<dyn ExtensionMetrics>>,
        _session_store: &'a ExtensionData,
        _thread_store: &'a ExtensionData,
        _turn_store: &'a ExtensionData,
    ) -> ExtensionFuture<'a, Vec<Box<dyn ContextualUserFragment + Send>>> {
        Box::pin(async move {
            let query = user_text(&input.user_input);
            if query.is_empty() {
                return Vec::new();
            }
            match self.memory.recall(&query, AUTO_RECALL_LIMIT).await {
                Ok(mut memories) => {
                    if memories.is_empty() {
                        return Vec::new();
                    }
                    let scope = self.memory.scope();
                    bound_context(scope, &query, &mut memories);
                    if memories.is_empty() {
                        return Vec::new();
                    }
                    let body = serde_json::to_string(&FactoryMemoryContext {
                        source: "factory-qdrant-memory",
                        namespace: scope.namespace(),
                        repository_id: scope.repository_id(),
                        query: &query,
                        memories: &memories,
                    })
                    .unwrap_or_else(|error| format!("{{\"serialization_error\":{error:?}}}"));
                    vec![Box::new(FactoryMemoryContextFragment { body })
                        as Box<dyn ContextualUserFragment + Send>]
                }
                Err(error) => {
                    let scope = self.memory.scope();
                    let body = json!({
                        "source": "factory-qdrant-memory",
                        "namespace": scope.namespace(),
                        "repository_id": scope.repository_id(),
                        "error": error.to_string(),
                    })
                    .to_string();
                    vec![Box::new(FactoryMemoryErrorFragment { body })
                        as Box<dyn ContextualUserFragment + Send>]
                }
            }
        })
    }
}

impl ContextualUserFragment for FactoryMemoryContextFragment {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn requires_separate_message(&self) -> bool {
        true
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn body(&self) -> String {
        self.body.clone()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<factory_memory_context>", "</factory_memory_context>")
    }
}

impl ContextualUserFragment for FactoryMemoryErrorFragment {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn requires_separate_message(&self) -> bool {
        true
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn body(&self) -> String {
        self.body.clone()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<factory_memory_error>", "</factory_memory_error>")
    }
}

fn user_text(input: &[UserInput]) -> String {
    input
        .iter()
        .filter_map(|item| match item {
            UserInput::Text { text, .. } if !text.trim().is_empty() => Some(text.trim()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn bound_context(scope: &FactoryMemoryScope, query: &str, memories: &mut Vec<FactoryMemoryHit>) {
    while !memories.is_empty() {
        let length = serde_json::to_vec(&FactoryMemoryContext {
            source: "factory-qdrant-memory",
            namespace: scope.namespace(),
            repository_id: scope.repository_id(),
            query,
            memories,
        })
        .map_or(usize::MAX, |body| body.len());
        if length <= MAX_MEMORY_CONTEXT_BYTES {
            break;
        }
        memories.pop();
    }
}

fn validate_tags(tags: &mut [String]) -> Result<(), String> {
    let mut unique = HashSet::new();
    for tag in tags {
        require_text("memory tag", tag)?;
        if !unique.insert(tag.as_str()) {
            return Err(format!("duplicate memory tag {tag}"));
        }
    }
    Ok(())
}

fn require_text(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} must not be empty"))
    } else {
        Ok(())
    }
}

fn parse_args<T: for<'de> Deserialize<'de>>(call: &ToolCall) -> Result<T, FunctionCallError> {
    serde_json::from_str(call.function_arguments()?).map_err(|error| respond(error.to_string()))
}

fn json_output(value: impl Serialize) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
    let value =
        serde_json::to_value(value).map_err(|error| FunctionCallError::Fatal(error.to_string()))?;
    Ok(Box::new(JsonToolOutput::new(value)))
}

fn respond(message: impl Into<String>) -> FunctionCallError {
    FunctionCallError::RespondToModel(message.into())
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn remember_schema() -> Value {
    object(
        json!({
            "content": {
                "type": "string",
                "description": "Self-contained information worth recalling in later threads.",
                "maxLength": MAX_MEMORY_CONTENT_BYTES,
            },
            "tags": {
                "type": "array",
                "description": "Short labels that make the memory easier to inspect.",
                "items": {"type": "string"},
            },
        }),
        &["content", "tags"],
    )
}

fn recall_schema() -> Value {
    object(
        json!({
            "query": {"type": "string", "description": "Lexical memory search query."},
            "limit": {
                "type": ["integer", "null"],
                "minimum": 1,
                "maximum": MAX_RECALL_LIMIT,
                "description": "Maximum ranked memories to return; null uses the default.",
            },
        }),
        &["query", "limit"],
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use codex_extension_api::NoopTurnItemEmitter;
    use codex_extension_api::ToolPayload;
    use codex_utils_output_truncation::TruncationPolicy;

    use super::*;
    use crate::memory::FactoryMemoryError;
    use crate::memory::FactoryMemoryFuture;
    use crate::memory::FactoryMemoryRecord;
    use crate::memory::FactoryMemoryStore;

    #[derive(Default)]
    struct LeakyMemoryStore {
        next_id: AtomicUsize,
        memories: Mutex<Vec<FactoryMemoryRecord>>,
    }

    impl FactoryMemoryStore for LeakyMemoryStore {
        fn remember<'a>(
            &'a self,
            scope: &'a FactoryMemoryScope,
            source_thread_id: &'a str,
            content: String,
            tags: Vec<String>,
        ) -> FactoryMemoryFuture<'a, FactoryMemoryRecord> {
            Box::pin(async move {
                let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
                let memory = FactoryMemoryRecord {
                    id: format!("memory-{id}"),
                    content,
                    namespace: scope.namespace().to_string(),
                    repository_id: scope.repository_id().to_string(),
                    tags,
                    source_thread_id: source_thread_id.to_string(),
                    created_at: "2026-08-02T00:00:00.000Z".to_string(),
                    updated_at: "2026-08-02T00:00:00.000Z".to_string(),
                    vectorizer: "test".to_string(),
                };
                self.memories
                    .lock()
                    .expect("memory store lock")
                    .push(memory.clone());
                Ok(memory)
            })
        }

        fn recall<'a>(
            &'a self,
            _scope: &'a FactoryMemoryScope,
            _query: &'a str,
            limit: usize,
        ) -> FactoryMemoryFuture<'a, Vec<FactoryMemoryHit>> {
            Box::pin(async move {
                // Intentionally return every repository's records. The scoped
                // capability must still prevent them from reaching a caller.
                Ok(self
                    .memories
                    .lock()
                    .expect("memory store lock")
                    .iter()
                    .take(limit)
                    .cloned()
                    .map(|memory| FactoryMemoryHit { memory, score: 4.0 })
                    .collect())
            })
        }
    }

    fn memory_extension(memory: &FactoryMemory, repository_id: &str) -> FactoryMemoryExtension {
        memory_extension_at_stage(memory, repository_id, FactoryTurnStage::Execute)
    }

    fn memory_extension_at_stage(
        memory: &FactoryMemory,
        repository_id: &str,
        stage: FactoryTurnStage,
    ) -> FactoryMemoryExtension {
        FactoryMemoryExtension {
            memory: memory.for_repository(
                FactoryRepositoryId::new(repository_id).expect("repository identity"),
            ),
            stage,
        }
    }

    fn memory_tool(
        extension: &FactoryMemoryExtension,
        thread_id: &str,
        name: &str,
    ) -> Arc<dyn ToolExecutor<ToolCall>> {
        extension
            .tools(
                &ExtensionData::new("session"),
                &ExtensionData::new(thread_id),
            )
            .into_iter()
            .find(|tool| tool.tool_name() == ToolName::plain(name))
            .unwrap_or_else(|| panic!("missing memory tool {name}"))
    }

    fn tool_call(name: &str, arguments: Value) -> (ToolCall, ToolPayload) {
        tool_call_at_stage(name, arguments, FactoryTurnStage::Execute)
    }

    fn tool_call_at_stage(
        name: &str,
        arguments: Value,
        stage: FactoryTurnStage,
    ) -> (ToolCall, ToolPayload) {
        let payload = ToolPayload::Function {
            arguments: arguments.to_string(),
        };
        (
            ToolCall {
                turn_id: "turn-1".to_string(),
                call_id: "call-1".to_string(),
                tool_name: ToolName::plain(name),
                model: "test-model".to_string(),
                codex_turn_metadata: Some(
                    json!({
                        crate::FACTORY_STAGE_METADATA_KEY:
                            stage.as_wire_name(),
                    })
                    .to_string(),
                ),
                truncation_policy: TruncationPolicy::Bytes(16_384),
                conversation_history: codex_extension_api::ConversationHistory::default(),
                turn_item_emitter: Arc::new(NoopTurnItemEmitter),
                environments: Vec::new(),
                payload: payload.clone(),
            },
            payload,
        )
    }

    async fn recall_receipt(extension: &FactoryMemoryExtension) -> Value {
        let tool = memory_tool(extension, "thread-recall", RECALL_TOOL);
        let (call, payload) = tool_call(
            RECALL_TOOL,
            json!({"query": "repository alpha nonce", "limit": 5}),
        );
        tool.handle(call)
            .await
            .expect("recall succeeds")
            .post_tool_use_response("call-1", &payload)
            .expect("recall response")
    }

    async fn automatic_recall(extension: &FactoryMemoryExtension) -> Vec<String> {
        extension
            .contribute(
                TurnInputContext {
                    turn_id: "turn-auto".to_string(),
                    user_input: vec![UserInput::Text {
                        text: "repository alpha nonce".to_string(),
                        text_elements: Vec::new(),
                    }],
                    environments: Vec::new(),
                },
                None,
                &ExtensionData::new("session"),
                &ExtensionData::new("thread-auto"),
                &ExtensionData::new("turn-auto"),
            )
            .await
            .into_iter()
            .map(|fragment| fragment.render())
            .collect()
    }

    #[tokio::test]
    async fn plan_cannot_persist_long_term_memory() {
        let store = Arc::new(LeakyMemoryStore::default());
        let memory =
            FactoryMemory::with_store("factory-global", store.clone()).expect("memory capability");
        let extension =
            memory_extension_at_stage(&memory, "local:repository-a", FactoryTurnStage::Plan);
        let tools = extension.tools(
            &ExtensionData::new("session"),
            &ExtensionData::new("thread-plan"),
        );
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_name(), ToolName::plain(RECALL_TOOL));
        assert!(store.memories.lock().expect("memory store lock").is_empty());
    }

    #[test]
    fn ordinary_subagents_receive_recall_but_not_memory_writes() {
        let store: Arc<dyn FactoryMemoryStore> = Arc::new(LeakyMemoryStore::default());
        let memory = FactoryMemory::with_store("factory-global", store).expect("memory capability");
        let extension = memory_extension(&memory, "local:repository-a");
        let thread_store = ExtensionData::new("thread-child");
        thread_store.insert(FactoryThreadScope::Subagent);

        let tools = extension.tools(&ExtensionData::new("session"), &thread_store);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_name(), ToolName::plain(RECALL_TOOL));
    }

    #[tokio::test]
    async fn explicit_and_automatic_memory_are_repository_scoped() {
        let store = Arc::new(LeakyMemoryStore::default());
        let store: Arc<dyn FactoryMemoryStore> = store;
        let memory = FactoryMemory::with_store("factory-global", store).expect("memory capability");
        let repository_a = memory_extension(&memory, "local:repository-a");
        let repository_b = memory_extension(&memory, "local:repository-b");

        let remember = memory_tool(&repository_a, "thread-a", REMEMBER_TOOL);
        let (call, payload) = tool_call(
            REMEMBER_TOOL,
            json!({
                "content": "repository alpha nonce belongs only to A",
                "tags": ["isolation"],
            }),
        );
        let receipt = remember
            .handle(call)
            .await
            .expect("remember succeeds")
            .post_tool_use_response("call-1", &payload)
            .expect("remember response");
        assert_eq!(receipt["repository_id"], "local:repository-a");

        let same_repository = recall_receipt(&repository_a).await;
        assert_eq!(same_repository["count"], 1);
        assert_eq!(
            same_repository["memories"][0]["repository_id"],
            "local:repository-a"
        );

        let different_repository = recall_receipt(&repository_b).await;
        assert_eq!(different_repository["count"], 0);
        assert_eq!(different_repository["memories"], json!([]));

        let same_repository_context = automatic_recall(&repository_a).await;
        assert_eq!(same_repository_context.len(), 1);
        assert!(same_repository_context[0].contains("belongs only to A"));
        assert!(same_repository_context[0].contains("local:repository-a"));

        assert!(automatic_recall(&repository_b).await.is_empty());
    }

    #[test]
    fn repository_identity_rejects_empty_values() {
        assert_eq!(
            FactoryRepositoryId::new("  ")
                .expect_err("empty identity")
                .to_string(),
            FactoryMemoryError::new("Factory repository identity must not be empty").to_string()
        );
    }
}
