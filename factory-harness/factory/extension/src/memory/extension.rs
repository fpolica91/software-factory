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
use crate::memory::FactoryMemoryStore;

const REMEMBER_TOOL: &str = "factory_remember";
const RECALL_TOOL: &str = "factory_recall";
const AUTO_RECALL_LIMIT: usize = 5;
const MAX_RECALL_LIMIT: usize = 8;
const MAX_MEMORY_CONTENT_BYTES: usize = 4096;
const MAX_MEMORY_CONTEXT_BYTES: usize = 12_000;

#[derive(Clone)]
struct FactoryMemoryExtension {
    memory: FactoryMemory,
}

#[derive(Clone, Copy)]
enum FactoryMemoryToolKind {
    Remember,
    Recall,
}

#[derive(Clone)]
struct FactoryMemoryToolExecutor {
    kind: FactoryMemoryToolKind,
    store: Arc<dyn FactoryMemoryStore>,
    namespace: Arc<str>,
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
    stored: bool,
    tag_count: usize,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct RecallReceipt {
    namespace: String,
    count: usize,
    memories: Vec<FactoryMemoryHit>,
}

#[derive(Debug, Serialize)]
struct FactoryMemoryContext<'a> {
    source: &'static str,
    namespace: &'a str,
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
) {
    let extension = Arc::new(FactoryMemoryExtension { memory });
    registry.tool_contributor(extension.clone());
    registry.turn_input_contributor(extension);
}

impl ToolContributor for FactoryMemoryExtension {
    fn tools(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
    ) -> Vec<Arc<dyn ToolExecutor<ToolCall>>> {
        let store = self.memory.store();
        let namespace: Arc<str> = self.memory.namespace().to_string().into();
        let source_thread_id: Arc<str> = thread_store.level_id().to_string().into();
        [
            FactoryMemoryToolKind::Remember,
            FactoryMemoryToolKind::Recall,
        ]
        .into_iter()
        .map(|kind| {
            Arc::new(FactoryMemoryToolExecutor {
                kind,
                store: Arc::clone(&store),
                namespace: Arc::clone(&namespace),
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
                FactoryMemoryToolKind::Remember => self.remember(call).await,
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
            .store
            .remember(
                &self.namespace,
                &self.source_thread_id,
                args.content,
                args.tags,
            )
            .await
            .map_err(|error| respond(error.to_string()))?;
        json_output(RememberReceipt {
            id: memory.id,
            namespace: memory.namespace,
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
            .store
            .recall(&self.namespace, &args.query, limit)
            .await
            .map_err(|error| respond(error.to_string()))?;
        json_output(RecallReceipt {
            namespace: self.namespace.to_string(),
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
                "Persist one durable Factory memory in the configured namespace for later threads. Store only information worth recalling beyond this turn.",
                remember_schema(),
            ),
            Self::Recall => (
                "Search durable Factory memory in the configured namespace and return ranked records with their source thread and tags.",
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
            match self
                .memory
                .store()
                .recall(self.memory.namespace(), &query, AUTO_RECALL_LIMIT)
                .await
            {
                Ok(mut memories) => {
                    if memories.is_empty() {
                        return Vec::new();
                    }
                    bound_context(self.memory.namespace(), &query, &mut memories);
                    if memories.is_empty() {
                        return Vec::new();
                    }
                    let body = serde_json::to_string(&FactoryMemoryContext {
                        source: "factory-qdrant-memory",
                        namespace: self.memory.namespace(),
                        query: &query,
                        memories: &memories,
                    })
                    .unwrap_or_else(|error| format!("{{\"serialization_error\":{error:?}}}"));
                    vec![Box::new(FactoryMemoryContextFragment { body })
                        as Box<dyn ContextualUserFragment + Send>]
                }
                Err(error) => {
                    let body = json!({
                        "source": "factory-qdrant-memory",
                        "namespace": self.memory.namespace(),
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

fn bound_context(namespace: &str, query: &str, memories: &mut Vec<FactoryMemoryHit>) {
    while !memories.is_empty() {
        let length = serde_json::to_vec(&FactoryMemoryContext {
            source: "factory-qdrant-memory",
            namespace,
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
