# Factory Native State Extension

This crate adds Factory decomposition, progress, review, and remediation state
to the existing Codex thread lifecycle. Four direct model tools update typed
state, and a turn-context contributor injects the latest JSON snapshot into
the same thread. Codex continues to own sampling, tools, turns, and history.

`FactoryStateBackend` is the persistence boundary. It loads and saves by the
stable Codex thread ID; `FactorydStateBackend` provides durable storage without
changing contributors or tool contracts.

The default `InMemoryFactoryStateBackend` is intentionally process-local. It
can restore a recreated thread while the same `factory-runtime` process is
alive, but it is **not crash durability** and is not a substitute for the
factoryd-backed runtime profile.

`FactorydStateBackend` uses factoryd's `GET` and `PUT
/v1/threads/{threadId}/state` API. It stores definitions and the native state
revision in `decomposition`, per-unit status in `progress`, the typed review in
`review`, typed disposition records in `remediation`, and native collaboration
activity in `subagents`. A missing record
loads as empty state; other HTTP or decoding failures are returned to the
extension. `factory-runtime` selects this durable backend when `FACTORYD_URL`
is set and otherwise keeps the process-memory standalone behavior.

## Native subagents

Factory uses Codex's existing collaboration primitives rather than defining an
agent loop or replacement tool. A turn-item contributor projects both
`CollabAgentToolCall` and V2 `SubAgentActivity` lifecycle items into durable
Factory state. Records retain the call and turn IDs, sender and receiver thread
IDs, native operation, available prompt, call status, and known child terminal
message/status. Started and completed observations upsert the same call.

Turn context tells the model to delegate independent runnable work, avoid
duplicate assignments, and reconcile results into progress, review, and
remediation. Persisted history remains complete; prompt injection is capped at
24 subagent activities and the latest 20 remediations, with long activity text
truncated, so Factory state does not undo Codex compaction.

## Long-term memory

When configured, the native memory extension exposes `factory_remember` and
`factory_recall`. A turn-input contributor derives its query from the actual
typed user text and injects bounded results inside
`<factory_memory_context>...</factory_memory_context>` before model execution.
Memories include a stable ID, content, namespace, tags, source Codex thread,
timestamps, and the vectorizer identity.

Qdrant is the V1 store. `LexicalSparseVectorizer` uses deterministic hashed
term-frequency dimensions and Qdrant's named sparse-vector API, so no embedding
service is required. `MemoryVectorizer` is an async sparse/dense boundary; a
later Ollama implementation can be optional without changing tools or recall.

Memory is enabled only when `FACTORY_QDRANT_URL` is set. Optional configuration
is `FACTORY_QDRANT_API_KEY`, `FACTORY_QDRANT_COLLECTION` (default
`factory_memories`), and `FACTORY_MEMORY_NAMESPACE` (default `default`). When
the URL is absent, memory tools and retrieval are not installed; Codex and all
other Factory extensions continue normally.
