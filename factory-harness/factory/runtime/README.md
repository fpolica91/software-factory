# Factory Worker Runtime

The `factory-runtime` crate is the library behind the shipped `factory-worker`
binary. The worker embeds the full Codex app-server lifecycle and installs
native Factory contributors; it does not implement a second model or agent
loop.

Every operation uses a fenced `FactorydStateBackend`. `FACTORYD_URL` identifies
that durable state service; there is no standalone process-memory product
mode.

Long-term memory is enabled by setting:

```sh
FACTORY_QDRANT_URL=http://127.0.0.1:6333
FACTORY_QDRANT_COLLECTION=factory_memories
FACTORY_MEMORY_NAMESPACE=default
```

`FACTORY_QDRANT_API_KEY` is optional. If `FACTORY_QDRANT_URL` is absent, the
runtime reports that memory is disabled, omits `factory_remember` and
`factory_recall`, and otherwise runs the complete Codex harness unchanged.
Qdrant sparse lexical retrieval is the baseline; Ollama is not required. The
worker passes the durable job repository identity into each Codex session, so
automatic and explicit recall remain repository-scoped even when the configured
namespace is shared by every worker.

Planning, execution, and remediation run autonomously inside the isolated job
container. Plan uses Codex's read-only Landlock path, removes `apply_patch` and
`request_permissions` before sampling, and still allows native shell and
subagent inspection. Its stage validator additionally requires a pure
decomposition. Native Rust functional acceptance is recorded in
`RUST_CUTOVER.md`; the superseded JavaScript fixtures have been deleted.

Each execution environment mounts only its exact managed worktree and matching
repository Git common directory at the absolute paths selected for Codex turns.
Docker, the default backend, derives those scoped mounts from the worker's
broader `/workspaces` backing mount. The optional Kubernetes backend uses one
plain Pod and PVC subpaths. Its default `local` workspace mode uses a static
local PV and is deliberately single-node. Its `existing-pvc` mode supports
multi-node execution scheduling through an operator-owned `ReadWriteMany` PVC
and a matching shared host mount; it does not make the Compose control plane
highly available. That shared filesystem must already allow the configured
`FACTORY_RUN_AS_UID` and `FACTORY_RUN_AS_GID` to access every required
subpath. Factory preserves operator ownership and omits Pod `fsGroup` in this
mode while retaining `runAsUser` and `runAsGroup`. The Compose worker uses
host networking and a copied, unprivileged kubeconfig; it does not receive the
Docker socket. An optional RuntimeClass name is passed through after launcher
preflight verifies the class and reports its handler.
Kubernetes execution requires an immutable cluster-reachable
`registry/repository@sha256:<64 lowercase hex>` reference. Its conservative
supported subset accepts a lowercase DNS/IPv4-style registry with an optional
numeric port and lowercase repository components separated by single `.`, `_`,
or `-` characters; bracketed IPv6 and tag+digest references are unsupported.
On Pod-producing startup, the launcher enforces this invariant before changing
the backend marker, workspace, or cluster. The Rust runtime independently
validates the reference during configuration normalization and again
immediately before Pod construction. Both the cluster-default runc path and the
optional operator-installed Kata RuntimeClass have passed full real-model
lifecycles. Factory does not install Kata.
The launcher persists the backend choice per installation. Kubernetes is a
fresh-install/separate-Compose-project profile, not an in-place migration from
Docker workspaces or PostgreSQL volumes.

A recovered Plan first promotes a valid completed checkpoint instead of
discarding durable work. Only after a replacement turn is proven necessary does
the runtime restore the recorded Factory state and managed-worktree baseline,
immediately before starting that turn. The current attempt's thread correlation
is stored before any fenced Plan or review recovery write.

Normal Plan restoration preserves the mounted worktree directory inode. If the
linked worktree is missing or corrupt, Plan preflight removes the old backend,
recreates the worktree explicitly, and provisions the same durable environment
identity and generation before starting a new Codex session.

Cancellation, provider failure, validation failure, and worker shutdown close
and drain the native Codex session before rollback. Detached review snapshots
preserve tracked, staged, and nonignored untracked content while allowing
ignored build artifacts. A review mutation is restored and rejected, with a
durable marker retaining that fact across process death until Factory review
state has also rolled back. A running job remains `cancelling` until this cleanup
finishes; only then does the worker acknowledge its terminal cancellation.
Compose gives `factory-worker` a 75-second stop grace period so Docker does not
force-kill it during the app-server drain, rollback, and immediate lease
relinquishment path.
