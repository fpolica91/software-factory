# Factory Native State Extension

This crate adds Factory decomposition, progress, review, and remediation state
to the existing Codex thread lifecycle. Four direct model tools update typed
state, and a turn-context contributor injects the latest JSON snapshot into
the same thread. Codex continues to own sampling, tools, turns, and history.

`FactoryStateBackend` is the persistence boundary. It loads and saves by the
stable Codex thread ID. The shipped worker always supplies a fenced
`FactorydStateBackend`; changing the backend does not change contributor or
tool contracts.

`FactorydStateBackend` uses factoryd's `GET` and `PUT
/threads/{threadId}/state` API. It stores definitions and the native state
revision in `decomposition`, per-unit status in `progress`, the typed review in
`review`, typed disposition records in `remediation`, completed rejected-review
and remediation cycles in `reviewHistory`, and native collaboration activity in
`subagents`. Detailed collaboration snapshots are first archived as fenced
`factory.subagent.activity` job events. The state document keeps the latest
event cursor plus an active/recent projection. Each snapshot uses a stable,
job-scoped identity, so retrying archival cannot duplicate history. Existing
unbounded documents receive a one-time idempotent backfill, and projection
entries are removed only after every archived entry is acknowledged. A missing
record loads as empty state; other HTTP or decoding failures are returned to
the extension.
`factory-worker` constructs this durable backend for each operation from its
required `FACTORYD_URL`. There is no process-memory production fallback.

## Native subagents

Factory uses Codex's existing collaboration primitives rather than defining an
agent loop or replacement tool. A turn-item contributor projects both
`CollabAgentToolCall` and `SubAgentActivity` lifecycle items into durable
Factory state. Records retain the call and turn IDs, sender and receiver thread
IDs, native operation, available prompt, call status, and known child terminal
message/status. Started and completed observations upsert the same call.

Turn context tells the model to delegate independent runnable work, avoid
duplicate assignments, and reconcile results into progress, review, and
remediation. The durable job-event stream remains the complete audit history.
The current state document retains at most 24 latest-per-child/recent
activities only after that event archive succeeds; a backend without a
separate event substrate retains every activity. Prompt injection applies the
same 24-activity cap and the latest 20 remediations, with long activity text
truncated, so Factory state does not undo Codex compaction.

## Long-term memory

When configured, the native memory extension exposes `factory_remember` and
`factory_recall`. A turn-input contributor derives its query from the actual
typed user text and injects bounded results inside
`<factory_memory_context>...</factory_memory_context>` before model execution.
Memories include a stable ID, content, deployment namespace, durable repository
identity, tags, source Codex thread, timestamps, and the vectorizer identity.
Both automatic recall and the explicit tools always store and query within the
current job repository. A different repository cannot receive those records,
even when both jobs use the same Qdrant collection and deployment namespace.

Qdrant is the current long-term-memory store. `LexicalSparseVectorizer` uses
deterministic hashed term-frequency dimensions and Qdrant's named sparse-vector
API, so no embedding service is required. Lexical recall requires at least two
matching weighted terms; a one-word collision is not injected into a turn.
`MemoryVectorizer` is an async sparse/dense boundary; a later implementation can
remain optional without changing tools or recall.

Memory is enabled only when `FACTORY_QDRANT_URL` is set. Optional configuration
is `FACTORY_QDRANT_API_KEY`, `FACTORY_QDRANT_COLLECTION` (default
`factory_memories`), and `FACTORY_MEMORY_NAMESPACE` (default `default`). The
namespace partitions deployments; it does not enable cross-repository sharing.
When the URL is absent, memory tools and retrieval are not installed; Codex and
all other Factory extensions continue normally.
