# CleanRL GPU Benchmark

This directory defines a reproducible two-host Software Factory benchmark
against pinned CleanRL commit
`fe8d8a03c41a7ef5b523e2e354bd01c363e786bb`. GLM-5.2 implements two issue
candidates in isolated Kubernetes workspaces, then each GPU runs a real C51
train-checkpoint-evaluate loop. [The report](REPORT.md) summarizes completed,
contract-validated receipts only, with the same four rows rendered as
[PNG](charts/benchmark-summary.png) and
[SVG](charts/benchmark-summary.svg).

The issue gates are deliberately node-specific:

- #488 runs its focused ToyText regression and real `FrozenLake-v1` PyTorch
  PPO training on the `linux/arm64` NVIDIA GB10 profile.
- #562 runs its focused dummy-step/GAE regression and real `Breakout-v5`
  PyTorch EnvPool PPO training on the `linux/amd64` NVIDIA A100 profile.

Product validation is separate: the manifest defines two otherwise matched
`cleanrl/c51.py` `CartPole-v1` runs, one per profile. CleanRL selects CUDA;
GPU telemetry is sampled while that process is active. `--save-model` retains
the checkpoint and starts the upstream ten-episode evaluation. Fresh
reproductions first execute the manifest's real CUDA backward plus optimizer
step, assert that both `Linear.weight` and `Linear.bias` changed, and end their
receipt with these four exact lines (using the profile's exact `gpu_name`):

```text
GPU name: NVIDIA GB10
weight changed: True
bias changed: True
CUDA PREFLIGHT PASS
```

The A100 receipt substitutes `NVIDIA A100-SXM4-40GB` on the first line.
The two measured jobs predate this deterministic producer format. The GB10
run retained an asserted weight/bias-change receipt; the A100 run retained
positive parameter deltas. The collector validates those two run-specific
legacy formats as well as the deterministic format above. Fresh runs must emit
the deterministic format.

## Prepare and run

This evidence plan pins Z.AI's coding plan and GLM-5.2. Reuse the key already
stored by Factory; do not place it in the command or manifest:

```sh
factory configure --provider zai --model glm-5.2 --base coding
```

Every execution profile fixes the namespace, node, RuntimeClass, GPU request,
and exact image digest used by the run. The matching public multi-architecture
image is `ghcr.io/fpolica91/software-factory:gpu-pytorch-25.08`; its per-platform
digests are recorded in `public_image_digest` for independent reproduction.

Dependencies are locked independently for the two Python boundaries:

- GB10 creates a Pod-local Python 3.12 environment with
  `--system-site-packages`, preserving the NVIDIA image's CUDA Torch, and
  exposes it through the workspace `.venv` symlink. Its userspace input is
  `requirements/gb10.in`; `requirements/gb10.txt` was generated for ARM64
  with hashes and deliberately contains no Torch package. Recreate it with:

  ```sh
  uv pip compile --generate-hashes --python-version 3.12 \
    --python-platform aarch64-manylinux_2_28 \
    benchmarks/cleanrl-gpu/requirements/gb10.in \
    --output-file benchmarks/cleanrl-gpu/requirements/gb10.txt
  ```

- A100 uses Python 3.10.16 and CleanRL's own `uv.lock` from the pinned source
  commit. Its uv cache and environment live under Pod-local `/tmp`, with
  `.venv` as a workspace symlink; this avoids writing dependency trees across
  the shared NFS workspace. Use the exact commands in `run-manifest.json` and
  do not resolve a second dependency set.

Both profiles keep TensorBoard's high-frequency `runs/` writes on Pod-local
storage. Git changes, benchmark logs, checkpoints, and evaluation receipts
remain on the durable workspace.

Each issue job uses its checked-in prompt under `prompts/`. The prompt requires
the focused regression, mandatory CUDA optimizer-step preflight, issue-specific
PPO run, matched C51 `--save-model` run, raw logs under
`.factory/benchmark/`, and a compact terminal summary.

The measured jobs began from earlier versions of those prompts. Each manifest
entry therefore keeps two exact allowlisted hashes: `submitted_task_sha256`
identifies the historical measured task, while `reproduction_task_sha256`
identifies the current prompt after shell command substitution removes its
trailing newline. The collector accepts no other task hash. This preserves the
original evidence without representing the improved reproduction prompt as
byte-identical.

## Launch the pinned workers and jobs

The manifest pins the exact digest for each measured run: the GB10 entry
records the node-local build, while the A100 entry records the public GHCR
image. The launch commands below use public per-platform images. For a fresh
GB10 collection, make an ignored reproduction copy, set `BENCHMARK_MANIFEST`
to its absolute path, and replace that copy's GB10 `execution_image` and
`image_reference` with the public digest-qualified reference below and its
`resolved_image_digest` with the matching `public_image_digest`. The contracts
accept either pinned pair; do not rewrite the checked-in measured manifest.

```sh
cp benchmarks/cleanrl-gpu/run-manifest.json \
  benchmarks/cleanrl-gpu/run-manifest.reproduction.json
export BENCHMARK_MANIFEST="$PWD/benchmarks/cleanrl-gpu/run-manifest.reproduction.json"
```

The two benchmark workers use the same provider and model, so launch them
sequentially to make node assignment deterministic. This assumes the existing
Kubernetes/PVC Factory profile is already configured. Set the common worker
contract once:

```sh
export FACTORY_WORKER_SLOTS=1
export FACTORY_KUBERNETES_NAMESPACE=software-factory-execution
export FACTORY_KUBERNETES_RUNTIME_CLASS=nvidia
export FACTORY_KUBERNETES_GPU_RESOURCE=nvidia.com/gpu
export FACTORY_KUBERNETES_GPU_COUNT=1
```

Start the GB10 worker and submit #488 detached. Record the printed job ID, then
run Phase A below while C51 is active; Phase B attaches through terminal
completion and collects the retained result.

```sh
export FACTORY_KUBERNETES_NODE_NAME=spark-91b3
export FACTORY_KUBERNETES_IMAGE=ghcr.io/fpolica91/software-factory@sha256:a1ee9c9920eb45cbe8362b6aa1b34c34207322b52b280b0a5428315e6d6c09a1
factory up
factory run --detach --no-apply --no-clarify \
  --repository https://github.com/vwxyzjn/cleanrl.git \
  --base-ref fe8d8a03c41a7ef5b523e2e354bd01c363e786bb \
  "$(cat benchmarks/cleanrl-gpu/prompts/issue-488.md)"
```

Only after #488 finishes Phase B, replace that worker configuration with A100
and submit #562 detached. Repeat Phase A and Phase B with its printed job ID.

```sh
export FACTORY_KUBERNETES_NODE_NAME=kent-ai-stuff
export FACTORY_KUBERNETES_IMAGE=ghcr.io/fpolica91/software-factory@sha256:ae8a2aac52a5fd32489e16812113092fe21c931b2167968a2fd93811c26c2c01
factory up
factory run --detach --no-apply --no-clarify \
  --repository https://github.com/vwxyzjn/cleanrl.git \
  --base-ref fe8d8a03c41a7ef5b523e2e354bd01c363e786bb \
  "$(cat benchmarks/cleanrl-gpu/prompts/issue-562.md)"
```

These commands contain no credentials. Factory reads the already configured
provider key without printing it. Confirm each execution Pod's `spec.nodeName`,
RuntimeClass, image, and GPU limit against the selected
`$BENCHMARK_MANIFEST` before accepting evidence.

## Collect safe aggregates

Collectors emit only verified profile identity plus whitelisted aggregates.
Run them from the benchmark directory in a Software Factory checkout. Set
`JOB_ID` to the actual Factory job and `C51_RUN_ID` to its matched C51 run; the
case statement fixes the issue ID and retained log path.

```sh
cd "$(git rev-parse --show-toplevel)/benchmarks/cleanrl-gpu"
: "${BENCHMARK_MANIFEST:=$PWD/run-manifest.json}"
: "${JOB_ID:?set JOB_ID to the running Factory job ID}"
: "${C51_RUN_ID:?set C51_RUN_ID to the matching manifest C51 run ID}"
: "${FACTORY_KUBERNETES_KUBECONFIG:?set the Factory Kubernetes kubeconfig path}"
export KUBECONFIG="$FACTORY_KUBERNETES_KUBECONFIG"
case "$C51_RUN_ID" in
  cleanrl-c51-gb10)
    ISSUE_RUN_ID=cleanrl-488-factory
    CUDA_LOG_PATH=.factory/benchmark/cuda-gb10.log
    FOCUSED_TEST_LOG_PATH=.factory/benchmark/issue-488-pytest.log
    PPO_LOG_PATH=.factory/benchmark/issue-488-ppo.log
    RL_LOG_PATH=.factory/benchmark/c51-gb10.log
    CHECKPOINT_PATH=.factory/benchmark/c51-gb10.cleanrl_model
    ;;
  cleanrl-c51-a100)
    ISSUE_RUN_ID=cleanrl-562-factory
    CUDA_LOG_PATH=.factory/benchmark/cuda-a100.log
    FOCUSED_TEST_LOG_PATH=.factory/benchmark/issue-562-pytest.log
    PPO_LOG_PATH=.factory/benchmark/issue-562-ppo.log
    RL_LOG_PATH=.factory/benchmark/c51-a100.log
    CHECKPOINT_PATH=.factory/benchmark/c51-a100.cleanrl_model
    ;;
  *) printf 'unknown C51 run ID: %s\n' "$C51_RUN_ID" >&2; exit 1 ;;
esac
ISSUE_OUT="data/$ISSUE_RUN_ID"
C51_OUT="data/$C51_RUN_ID"
mkdir -p "$ISSUE_OUT" "$C51_OUT"

PROFILE_SHOW=$(factory configure --show)
printf '%s\n' "$PROFILE_SHOW" |
  python3 scripts/collect.py profile --manifest "$BENCHMARK_MANIFEST" \
  --run-id "$ISSUE_RUN_ID" --output "$ISSUE_OUT/profile.json"
printf '%s\n' "$PROFILE_SHOW" |
  python3 scripts/collect.py profile --manifest "$BENCHMARK_MANIFEST" \
  --run-id "$C51_RUN_ID" --output "$C51_OUT/profile.json"
```

### Phase A: while C51 is running

Resolve exactly one Running execution Pod, then wait for its pinned C51
process. If the job terminates first, collection fails instead of sampling
setup or PPO work.

```sh

POD_NAME=
for _ in $(seq 1 180); do
  POD_NAMES=$(kubectl --namespace software-factory-execution get pods \
    --selector "software-factory.io/job-id=$JOB_ID" \
    --field-selector status.phase=Running \
    --output 'jsonpath={range .items[*]}{.metadata.name}{"\n"}{end}')
  set -- $POD_NAMES
  if [ "$#" -eq 1 ]; then
    POD_NAME=$1
    break
  elif [ "$#" -gt 1 ]; then
    printf 'found multiple Running execution Pods for %s\n' "$JOB_ID" >&2
    exit 1
  fi
  JOB_STATE=$(factory status "$JOB_ID" --json |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["job"]["job"]["state"])')
  case "$JOB_STATE" in
    succeeded|failed|cancelled)
      printf 'job ended before its execution Pod became Running\n' >&2
      exit 1
      ;;
  esac
  sleep 2
done
if [ -z "$POD_NAME" ]; then
  printf 'execution Pod did not become Running in time\n' >&2
  exit 1
fi

while ! kubectl --namespace software-factory-execution exec "$POD_NAME" -- \
  pgrep -f '^([^ ]*/)?python([0-9]+([.][0-9]+)*)?[ ]+[c]leanrl/c51[.]py([ ]|$).*--total-timesteps[ ]+500000([ ]|$)' >/dev/null 2>&1
do
  JOB_STATE=$(factory status "$JOB_ID" --json |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["job"]["job"]["state"])')
  case "$JOB_STATE" in
    succeeded|failed|cancelled)
      printf 'job ended before the C51 measurement window\n' >&2
      exit 1
      ;;
  esac
  sleep 5
done

kubectl --namespace software-factory-execution get pods \
  --selector "software-factory.io/job-id=$JOB_ID" \
  --field-selector status.phase=Running --output json |
  python3 scripts/collect.py kubernetes --manifest "$BENCHMARK_MANIFEST" \
  --run-id "$C51_RUN_ID" --job-id "$JOB_ID" \
  --output "$C51_OUT/kubernetes.json"

set -o pipefail
timeout 130s kubectl --namespace software-factory-execution exec "$POD_NAME" -- \
  sh -lc 'i=0; while [ "$i" -lt 120 ]; do
    pgrep -f '^([^ ]*/)?python([0-9]+([.][0-9]+)*)?[ ]+[c]leanrl/c51[.]py([ ]|$).*--total-timesteps[ ]+500000([ ]|$)' >/dev/null || exit 1
    nvidia-smi --query-gpu=timestamp,index,name,utilization.gpu,memory.used,power.draw --format=csv,noheader,nounits || exit 1
    i=$((i + 1))
    [ "$i" -eq 120 ] || sleep 1
  done' |
  tee "$C51_OUT/gpu-samples.csv" |
  python3 scripts/collect.py gpu --manifest "$BENCHMARK_MANIFEST" \
  --run-id "$C51_RUN_ID" --interval-seconds 1 --output "$C51_OUT/gpu.json"
```

The Kubernetes collector requires the manifest node, RuntimeClass, image,
image ID, one-GPU request/limit, PVC, and job-specific workspace mount. The GPU
collector requires exactly 120 guarded one-Hz rows for the single assigned GPU
and the exact name (`NVIDIA GB10` or `NVIDIA A100-SXM4-40GB`). It derives the
observed sample span from the first and last `nvidia-smi` timestamps; it does
not infer elapsed time from the requested interval.

### Phase B: only after terminal success

Attach until terminal. Whole-job Factory/model metrics use the issue ID; only
C51 hardware/RL metrics use the matched C51 ID.

```sh
factory attach "$JOB_ID"

FACTORYD_URL=${FACTORYD_URL:-http://127.0.0.1:8787}
factory status "$JOB_ID" --json |
  python3 scripts/collect.py factory --manifest "$BENCHMARK_MANIFEST" \
  --run-id "$ISSUE_RUN_ID" --job-id "$JOB_ID" \
  --factoryd-url "$FACTORYD_URL" --output "$ISSUE_OUT/factory.json"

JOB_STATE=$(factory status "$JOB_ID" --json |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["job"]["job"]["state"])')
if [ "$JOB_STATE" != succeeded ]; then
  printf 'benchmark job did not succeed: %s\n' "$JOB_STATE" >&2
  exit 1
fi

python3 scripts/collect.py model --manifest "$BENCHMARK_MANIFEST" \
  --run-id "$ISSUE_RUN_ID" \
  --factoryd-url "$FACTORYD_URL" \
  --job-id "$JOB_ID" --page-limit 1000 --output "$ISSUE_OUT/model.json"

: "${FACTORY_KUBERNETES_WORKSPACE_HOST_DIR:?set the verified shared workspace host path}"
CUDA_LOG="$FACTORY_KUBERNETES_WORKSPACE_HOST_DIR/jobs/$JOB_ID/$CUDA_LOG_PATH"
FOCUSED_TEST_LOG="$FACTORY_KUBERNETES_WORKSPACE_HOST_DIR/jobs/$JOB_ID/$FOCUSED_TEST_LOG_PATH"
PPO_LOG="$FACTORY_KUBERNETES_WORKSPACE_HOST_DIR/jobs/$JOB_ID/$PPO_LOG_PATH"
RL_LOG="$FACTORY_KUBERNETES_WORKSPACE_HOST_DIR/jobs/$JOB_ID/$RL_LOG_PATH"
CHECKPOINT="$FACTORY_KUBERNETES_WORKSPACE_HOST_DIR/jobs/$JOB_ID/$CHECKPOINT_PATH"
test -f "$CUDA_LOG"
test -f "$FOCUSED_TEST_LOG"
test -f "$PPO_LOG"
test -f "$RL_LOG"
test -s "$CHECKPOINT"
python3 scripts/collect.py issue --manifest "$BENCHMARK_MANIFEST" \
  --run-id "$ISSUE_RUN_ID" --cuda-input "$CUDA_LOG" \
  --pytest-input "$FOCUSED_TEST_LOG" --ppo-input "$PPO_LOG" \
  --output "$ISSUE_OUT/issue.json"
python3 scripts/collect.py rl --manifest "$BENCHMARK_MANIFEST" --run-id "$C51_RUN_ID" \
  --input "$RL_LOG" --cuda-input "$CUDA_LOG" --checkpoint "$CHECKPOINT" \
  --output "$C51_OUT/rl.json"
tail -n 4 "$CUDA_LOG" > "$C51_OUT/cuda-preflight.txt"
```

Current workers persist model usage from Codex's exact upstream-completion
event, including compaction requests, and ignore recomputed context snapshots.
For a job created by an older worker that omitted those events, pass one
`--rollout PATH` per job thread. The collector attributes each Codex rollout
to Factory `turn.started` events, rejects inconsistent cumulative counters,
and emits the same aggregate. Rollout JSONL contains conversation data: keep
it outside the repository and commit only the aggregate `model.json`.

Each issue row requires four sources: `profile`, `factory`, `model`, and
`issue`. Each C51 row requires four more: `profile`, `kubernetes`, `gpu`, and
`rl`. After both jobs, merge all sixteen observations into four scoped rows:

```sh
python3 scripts/collect.py merge data/cleanrl-488-factory/*.json \
  data/cleanrl-562-factory/*.json data/cleanrl-c51-gb10/*.json \
  data/cleanrl-c51-a100/*.json --manifest "$BENCHMARK_MANIFEST" \
  --output data/metrics.csv
```

The profile collector records `provider_base=coding` as a configuration
receipt without retaining a key; it is not job-execution provenance. The
Factory status and model-event requests independently validate the durable
job's provider and model. Each C51 data directory retains the safe six-column
`gpu-samples.csv` source trace and CUDA preflight receipt beside its normalized
JSON, so the hardware aggregate can be regenerated. `JOB_ID`, workspace
responses, raw prompts, reasoning, Pod names, private paths, failures, and
payload bodies remain private collector inputs and are never committed.

The managed workspace root is `/workspaces/jobs/JOB_ID`. For the verified
shared local/NFS profile, the launcher maps it to
`$FACTORY_KUBERNETES_WORKSPACE_HOST_DIR/jobs/JOB_ID`; collect the named log
there before workspace cleanup. `factory export` exports only the Git patch,
not arbitrary `.factory` logs. Other existing-PVC profiles need an explicit
PVC reader and are not covered by the host-path command above.

The RL collector rejects truncated output: it requires raw `global_step`
records, the exit-zero `factory_training_steps=500000` receipt, exactly
upstream `eval_episode` indices 0 through 9, and a nonempty checkpoint whose
byte count and SHA-256 are retained in `rl.json` and the merged CSV. The issue
collector separately retains the CUDA optimizer-step pass, focused-test count
and duration, and PPO completed steps, final SPS, and last training return.
Fresh PPO logs end with `factory_ppo_training_steps=TOTAL` only after exit 0.
That receipt proves the configured command completed; the collector reports
the algorithm's rollout-aligned completion separately from the last logged
training event. The retained pre-receipt GB10 log is accepted only because its
last logged step reaches that rollout-aligned boundary. These are real
execution gates, not mocked substitutes. PPO training returns remain
issue-execution evidence and must not be relabeled as evaluation.

GPU utilization is required for every sample. Memory and power are optional
paired metrics because NVIDIA unified-memory devices can report `[N/A]` for
`memory.used`; unavailable values are omitted, never converted to zero.

## Verify and render

Collection, validation, rendering, and tests use only the Python standard
library:

```sh
cd "$(git rev-parse --show-toplevel)"
python3 -m unittest discover -s benchmarks/cleanrl-gpu/tests -v
```

`data/metrics.csv` contains the four completed normalized rows. Rebuild the
committed PNG/SVG summaries deterministically with:

```sh
python3 benchmarks/cleanrl-gpu/scripts/render_charts.py \
  --manifest "${BENCHMARK_MANIFEST:-benchmarks/cleanrl-gpu/run-manifest.json}"
```

One matched seed is an end-to-end functional comparison, not a statistical
performance claim. Such claims require repeated matched seeds.
