# Factory Runtime Configuration

`factory-runtime` is the full Codex app-server lifecycle with additive native
Factory contributors. It does not implement a second model or agent loop.

`FACTORYD_URL` selects durable Factory thread state. Without it, state uses the
documented process-memory standalone backend.

Long-term memory is enabled by setting:

```sh
FACTORY_QDRANT_URL=http://127.0.0.1:6333
FACTORY_QDRANT_COLLECTION=factory_memories
FACTORY_MEMORY_NAMESPACE=default
```

`FACTORY_QDRANT_API_KEY` is optional. If `FACTORY_QDRANT_URL` is absent, the
runtime reports that memory is disabled, omits `factory_remember` and
`factory_recall`, and otherwise runs the complete Codex harness unchanged.
Qdrant sparse lexical retrieval is the baseline; Ollama is not required.

## Functional memory acceptance

With the GLM provider bridge listening on port 18102 and the Factory client
built, run from this directory:

```sh
node scripts/glm-qdrant-memory-smoke.mjs
```

The script owns a disposable `qdrant/qdrant:v1.16` container unless an external
`FACTORY_QDRANT_URL` is supplied. It stores a unique fact through
`factory_remember`, stops the complete runtime, starts a fresh runtime and
distinct Codex thread, proves automatic marked-context recall without a tool,
then verifies `factory_recall` and the exact persisted Qdrant payload/vector.
It also asserts that the fixture workspace remains byte-for-byte unchanged and
stops only the container it created.

## Functional subagent acceptance

With the same GLM bridge and built Factory client, run:

```sh
node scripts/glm-factoryd-subagent-smoke.mjs
```

The script starts disposable PostgreSQL and factoryd instances, then proves a
real parent model spawns a native child, waits for its exact result, and closes
or interrupts it. It restarts the complete runtime, verifies the unchanged
factoryd activity document, and inspects the persisted child through native
`thread/read` and parent-filtered `thread/list`. Only services started by the
script are stopped.
