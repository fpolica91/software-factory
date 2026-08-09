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
factory continue JOB_ID "Also rate-limit login"  # reopen a succeeded job
factory status JOB_ID                            # inspect current state
factory attach JOB_ID                            # reconnect to live output
factory attach --verbose JOB_ID                  # replay every event in full
factory result JOB_ID                            # print the complete result
factory artifacts JOB_ID                         # list fixed job artifacts
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

On an interactive terminal, `factory run` gates the task before submission:
the configured model reviews it and, when it is ambiguous, asks up to five
clarifying questions. Answers are appended to the task and the submitted
prompt is saved under `.factory/prompts/prompt_<id>.md`; a clear task is
submitted unchanged. Jobs themselves stay fully autonomous — clarification
happens only at this gate. Use `--no-clarify` to skip it; piped and `--json`
runs skip it automatically, and a gate failure falls back to submitting the
task as written.

Every terminal success prints the complete result before exiting. Local jobs
also write Factory-owned reports under `.factory/jobs/JOB_ID/`, outside the
managed worktree and result patch. Jobs created for a remote or different
checkout keep artifacts in coordinator storage; `factory result JOB_ID`
remains readable without entering a container.

A succeeded job is not closed to further work. `factory continue JOB_ID
"feedback"` reopens it durably: the feedback is appended to the durable task,
one iterate, review, remediate continuation round is appended to the same job,
and the same managed worktree and parent Codex thread continue where the
previous round finished. After an interactive `factory run` completes, Factory
also prompts once for follow-up feedback; entering text queues a continuation
round and reattaches, while Enter accepts the result. While a continuation
round is queued or running the job is no longer `succeeded`, so `apply` and
`export` become available again only after the round completes.

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
Status, stop, result, artifacts, apply, and export start only PostgreSQL plus
`factoryd`; completed results remain retrievable after a full shutdown without
a provider key or model worker. Provider/model configuration uses that same
control plane for the active-job check and never starts Qdrant, a provider
bridge, or a worker.

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

## Optional single-host K3s execution

Docker remains the default execution backend. The backend is immutable for one
Factory installation because Docker volumes and K3s host workspaces are not
automatically migrated. Use this profile only from a fresh checkout with its
own `FACTORY_PROJECT_NAME` and data volumes. On a Linux host with an existing
single-node K3s installation, set
`FACTORY_EXECUTION_ENVIRONMENT_BACKEND=kubernetes` in `.env` and review the
`FACTORY_KUBERNETES_*` values copied from `.env.example`. `factory up` validates
the user-readable kubeconfig and requires `FACTORY_KUBERNETES_IMAGE` to be a
cluster-reachable immutable
`registry/repository@sha256:<64 lowercase hex>` reference. The deliberately
conservative supported subset accepts a lowercase DNS/IPv4-style registry with
an optional numeric port and lowercase repository components separated by
single `.`, `_`, or `-` characters; bracketed IPv6 and tag+digest references
are unsupported. Both the launcher and Rust runtime enforce this invariant.
Invalid references fail before the backend marker, workspace, or cluster is
changed. The launcher then creates the host workspace directory, applies the
static local PV/PVC template, and starts the host-networked `factory-worker`.
PostgreSQL, Qdrant, `factoryd`, and the selected provider bridge remain shared
Compose services; only per-job Codex execution runs in Kubernetes Pods.

The profile is intentionally single-node because coordinator worktrees and
Pods share one host directory. An empty runtime class uses the K3s default.
By default the Compose project deterministically supplies a unique namespace,
PV, PVC, and host workspace directory. Explicit overrides remain supported but
must also be unique between installations. When a RuntimeClass is configured,
`factory up` verifies that it exists before starting the worker and prints its
exact class and handler. A missing or misspelled class therefore fails during
preflight instead of leaving execution Pods pending until timeout.

Live K3s/runc acceptance completed on Linux ARM64 with a real DeepSeek job. It
proved planning, remote commands and patches, native subagent delegation,
detached review, artifact materialization, apply, and `released/released` Pod
teardown through the same durable environment contract used by Docker.

### Optional Kata RuntimeClass

Kata remains an operator-installed runtime, not a Factory service. The pinned
`deploy/k3s/kata-qemu-runtime-rs.values.yaml` profile enables only
`kata-qemu-runtime-rs` on ARM64 and disables optional snapshotters and every
other shim. Installing it runs a privileged host-mounted DaemonSet that updates
K3s/containerd and may temporarily restart the node runtime; review that impact
before running:

```sh
FACTORY_KUBECONFIG=${FACTORY_KUBERNETES_KUBECONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/software-factory/k3s.yaml}

helm --kubeconfig "$FACTORY_KUBECONFIG" \
  upgrade --install kata-deploy \
  oci://ghcr.io/kata-containers/kata-deploy-charts/kata-deploy \
  --version 3.32.0 \
  --namespace kata-system --create-namespace \
  -f deploy/k3s/kata-qemu-runtime-rs.values.yaml
kubectl --kubeconfig "$FACTORY_KUBECONFIG" \
  -n kata-system rollout status daemonset/kata-deploy --timeout=20m
```

After RuntimeClass `kata-qemu-runtime-rs` exists, set
`FACTORY_KUBERNETES_RUNTIME_CLASS=kata-qemu-runtime-rs`; the next `factory up`
must report that class and its handler before the worker starts.

Definitive exact-source Kata acceptance passed on ARM64 from source fingerprint
`dec512b9…b8c3ce` with immutable image
`docker.io/library/software-factory@sha256:2bd920060b337573e8cbd751cc64c514174d2acdbad7a32f9f3c3caa6201611d`.
DeepSeek model `deepseek-v4-pro` completed all stages in job
`7003ae36-6f72-4d1a-830b-20f78c3cbeac`; Plan attempt 1 hit a fixture-only
`ImagePullBackOff` because the offline `k3s ctr images import` lacked the exact
digest alias. Adding that alias let durable attempt 2 recover; Execute, Review,
and Remediate each passed on attempt 1. This was local offline-import setup, not
a Factory retry bypass. Pod `factory-9a32720327d94a39a51c3121aeb9f269-g1`
(UID `519c1713-84d8-4b23-b05f-8aaa28895c3b`) used RuntimeClass
`kata-qemu-runtime-rs`; guest kernel 6.18.35 differed from host kernel
6.17.0-1014-nvidia. Environment `9a327203-27d9-4a39-a51c-3121aeb9f269`,
generation 1, ended `released/released` and the Pod was removed. Native-subagent
verification passed; attach, result, and apply succeeded; host `result.md` was
verified; and the sole applied file was `KATA_FINAL_ACCEPTANCE.txt`, exactly 14
bytes containing `KATA-FINAL-OK\n`.

Roll back with:

```sh
FACTORY_KUBECONFIG=${FACTORY_KUBERNETES_KUBECONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/software-factory/k3s.yaml}

helm --kubeconfig "$FACTORY_KUBECONFIG" \
  uninstall kata-deploy --namespace kata-system --wait --timeout 20m
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
