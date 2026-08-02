# Software Factory

Software Factory runs the native Codex agent as a durable, autonomous software
delivery job. Codex remains the only execution harness: it owns the model loop,
tools, threads, context compaction, resume, and approvals. Factory adds durable
job state, managed Git worktrees, checkpoints, retries, crash recovery, memory,
and the plan, execute, review, remediate, and re-review lifecycle.

The shipped application is Rust-only. One image contains four binaries:
`factory` (CLI), `factory-worker` (durable runner), `factoryd` (coordinator),
and `factory-provider-bridge` (optional non-Responses translation). There is no
Factory TypeScript client, Hatchet workflow, Cursor harness, or second agent
loop.

## Quick Start

Normal use requires Docker Engine with Compose and a Git repository. No host
agent CLI, database, Node installation, or Rust toolchain is required.

```sh
git clone https://github.com/fpolica91/software-factory.git
cd software-factory
./factory install
cd /path/to/your/project
factory
```

The first interactive run asks for a provider, API key, model, and task. The
installer places a symlink in `~/.local/bin`; add that directory to `PATH` if
needed. Factory pulls `ghcr.io/fpolica91/software-factory:edge`, starts the
baseline services, creates a managed worktree, submits the job, and streams its
durable events.

Jobs are autonomous by default. Attachment observes and controls a job; it is
not required to keep the job running.

```sh
factory run "Implement authentication"           # submit and attach
factory run --detach "Audit this repository"     # print the job ID and exit
factory run --repository https://host/org/repo.git --detach "Audit it"
factory status JOB_ID                            # inspect current state
factory attach JOB_ID                            # reconnect to live output
factory attach --verbose JOB_ID                  # replay every event in full
factory stop JOB_ID                              # cancel durably
factory apply JOB_ID                             # apply a completed result here
factory export JOB_ID -o result.patch            # preserve a portable patch
```

On an interactive terminal, run and attach use a bounded live view: lifecycle
pairs become one cell, arrows select a cell, and Enter or a mouse click opens
its full durable detail. The completed view closes automatically when untouched;
interacting pins it for inspection and `q` closes it. `Ctrl-C` restores the
terminal and detaches without stopping a running job. Pipes receive
deterministic compact text with no terminal escapes. Use `--verbose` for the
complete human event replay or `--json` for the unchanged NDJSON stream. Pass
`--detach` when a script should return immediately; otherwise non-interactive
runs stream until completion.

A successful attached run against a local repository applies its result to the
originating checkout by default; use `factory run --no-apply ...` to retain it
for explicit apply or export. Local work always targets the Git checkout where
the launcher is invoked: change into that checkout before running Factory.
`--repository` is only for a remote Git URL, not another host path.

Apply is deliberately conflict-free. Factory verifies the result digest, the
hashed host-repository identity, the job's immutable base commit, a completely
clean checkout, and Git's binary-patch preflight before changing files. It
preserves text and binary changes, additions, deletions, symlinks, and executable
mode. If any check fails, the completed job stays available and the host checkout
is unchanged. Detached jobs remain addressable by job ID when the launcher is
used from another repository.

Export paths are host paths, including absolute paths. Factory downloads and
verifies the complete patch before publishing it atomically without overwriting
an existing file; use `-o -` when raw patch bytes should go to standard output.
Status, stop, apply, and export start only PostgreSQL plus `factoryd`; completed
results remain retrievable after a full shutdown without a provider key or
model worker. Provider/model configuration uses that same control plane for
the active-job check and never starts Qdrant, a provider bridge, or a worker.

## Providers

Run `factory configure` for guided setup, `factory provider` to switch
providers, `factory model` to switch models, and `factory configure --show` to
review settings without displaying key values.

The provider and exact model are pinned into each job when it is created. Before
a provider or model change, Factory checks every queued, running, and cancelling
job. It refuses a switch that would leave a pinned or legacy unpinned job
without a matching worker and prints exact commands to inspect, stop, or serve
that job. `--force` overrides the refusal with a warning; it changes the
configuration used for new jobs only and does not stop or migrate existing
jobs.

OpenAI Responses is a direct Codex provider path. Anthropic, DeepSeek, and Z.AI
use the Rust provider bridge only for wire-format translation; the Codex model
and tool loop remains unchanged. Provider configuration also works
non-interactively through command flags and provider-specific key environment
variables. Exported keys are honored even when the local `.env` has no key. A
custom direct Responses-compatible endpoint is configured explicitly:

```sh
export OPENAI_API_KEY=your-key
factory configure --provider openai --model your-model \
  --base https://provider.example/v1
```

The supplied direct endpoint is persisted and used by Codex; it must implement
the OpenAI Responses API. Provider-specific details are under
[`docs/providers/`](docs/providers/).

## Services

The baseline Compose stack is deliberately small:

- PostgreSQL stores durable jobs, attempts, checkpoints, leases, correlations,
  events, and workspace state.
- Qdrant stores Factory long-term memory and retrieval data.
- `factoryd` exposes the durable coordinator API and owns managed worktrees.
- `factory-worker` claims jobs and runs the native Codex lifecycle.

Provider bridges start only for the selected `claude`, `deepseek`, or `zai`
profile. Redis (`coordination`), MinIO (`artifacts`), Ollama (`local-models`),
and Langfuse/ClickHouse (`observability`) remain explicit optional profiles;
none is required by the baseline job lifecycle.

```sh
factory up       # start or repair the stack
factory logs     # follow factoryd and worker logs
factory down     # stop services while preserving data
factory build    # maintainer-only local image build
```

## Source Layout

- `factory-harness/codex-rs/` preserves the upstream-shaped Codex kernel.
- `factory-harness/factory/runtime/` composes Codex and runs durable stages.
- `factory-harness/factory/extension/` implements Factory-native behavior and
  Qdrant memory.
- `factory-harness/factory/coordinator/` implements `factoryd`, persistence,
  recovery, event replay, and worktrees.
- `factory-harness/factory/cli/` implements the native job CLI.
- `factory-harness/factory/providers/` implements provider profiles and the
  optional Rust transport adapter.

See [`PRODUCT.md`](PRODUCT.md) for the product contract and
[`docs/adr/0001-codex-kernel-factory-extension.md`](docs/adr/0001-codex-kernel-factory-extension.md)
for the dependency boundary. Real-model acceptance now covers read-only
planning, tool-using execution, native subagent delegation, detached review,
remediation and re-review, detach/attach replay, managed worktrees, and
cross-job Qdrant recall. It also proves native token-triggered Codex compaction
continues the same review turn through later tool calls and terminal approval.
The combined recovery gate now passes as well: a non-graceful worker kill after
an in-flight execute checkpoint was reclaimed from the same durable attempt,
continued on the same parent thread, passed exact-output verification, and was
approved by an independent detached review. Exact evidence is recorded in
[`RUST_CUTOVER.md`](RUST_CUTOVER.md).
