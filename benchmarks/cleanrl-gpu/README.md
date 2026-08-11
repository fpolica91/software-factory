# CleanRL GPU Evidence Scaffold

This directory defines planned, reproducible evidence against pinned CleanRL
commit `fe8d8a03c41a7ef5b523e2e354bd01c363e786bb`. It does not contain benchmark
results.

The issue gates are deliberately node-specific:

- #488 runs its focused ToyText regression and real `FrozenLake-v1` PyTorch
  PPO training on the `linux/arm64` NVIDIA GB10 profile.
- #562 runs its focused dummy-step/GAE regression and real `Breakout-v5`
  PyTorch EnvPool PPO training on the `linux/amd64` NVIDIA A100 profile.

Product validation is separate: the manifest defines two otherwise matched
`cleanrl/c51.py` `CartPole-v1` runs, one per profile. `--save-model` makes each
run execute real CUDA training and the upstream ten-episode evaluation. Every
run first executes its profile's CUDA preflight, which performs backward and
optimizer steps and proves that a CUDA parameter changed.

## Prepare and run

This evidence plan pins Z.AI's coding plan and GLM-5.2. Reuse the key already
stored by Factory; do not place it in the command or manifest:

```sh
factory configure --provider zai --model glm-5.2 --base coding
```

Every execution profile fixes the namespace, node, RuntimeClass, GPU request,
and node-local image digest. `public_image_digest` remains `null` until the
multi-architecture image is published by Actions; the verified node-local
digest references are sufficient for these runs.

Dependencies are locked independently for the two Python boundaries:

- GB10 creates a Python 3.12 virtual environment with
  `--system-site-packages`, preserving the NVIDIA image's CUDA Torch. Its
  userspace input is `requirements/gb10.in`; `requirements/gb10.txt` was
  generated for ARM64 with hashes and deliberately contains no Torch package.
  Recreate it with:

  ```sh
  uv pip compile --generate-hashes --python-version 3.12 \
    --python-platform aarch64-manylinux_2_28 \
    benchmarks/cleanrl-gpu/requirements/gb10.in \
    --output-file benchmarks/cleanrl-gpu/requirements/gb10.txt
  ```

- A100 uses Python 3.10.16 and CleanRL's own `uv.lock` from the pinned source
  commit. Run
  `UV_PYTHON=3.10.16 uv sync --frozen --extra envpool --extra pytest --no-dev`;
  do not resolve a second dependency set.

Each issue job uses its checked-in prompt under `prompts/`. The prompt requires
the focused regression, mandatory CUDA optimizer-step preflight, issue-specific
PPO run, matched C51 `--save-model` run, raw logs under
`.factory/benchmark/`, and a compact terminal summary.

## Launch the pinned workers and jobs

The node-local tag must first have its digest-qualified alias in each node's
K3s containerd namespace. Run the matching command on each named node after
importing `software-factory-gpu:benchmark-2b88673`:

```sh
# spark-91b3
sudo k3s ctr images tag --force \
  docker.io/library/software-factory-gpu:benchmark-2b88673 \
  docker.io/library/software-factory-gpu@sha256:a5e2582363acdbf172e1ebac1c5ba1de87a0fcb7420d655263b0477fa9861557

# kent-ai-stuff
sudo k3s ctr images tag --force \
  docker.io/library/software-factory-gpu:benchmark-2b88673 \
  docker.io/library/software-factory-gpu@sha256:4d518b4a8304930a98ecd214ee580e360291c055219c1eaf0b1bef887a4b4673
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
export FACTORY_KUBERNETES_IMAGE=docker.io/library/software-factory-gpu@sha256:a5e2582363acdbf172e1ebac1c5ba1de87a0fcb7420d655263b0477fa9861557
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
export FACTORY_KUBERNETES_IMAGE=docker.io/library/software-factory-gpu@sha256:4d518b4a8304930a98ecd214ee580e360291c055219c1eaf0b1bef887a4b4673
factory up
factory run --detach --no-apply --no-clarify \
  --repository https://github.com/vwxyzjn/cleanrl.git \
  --base-ref fe8d8a03c41a7ef5b523e2e354bd01c363e786bb \
  "$(cat benchmarks/cleanrl-gpu/prompts/issue-562.md)"
```

These commands contain no credentials. Factory reads the already configured
provider key without printing it. Confirm each execution Pod's `spec.nodeName`,
RuntimeClass, image, and GPU limit against `run-manifest.json` before accepting
evidence.

## Collect safe aggregates

Collectors emit only verified profile identity plus whitelisted aggregates.
Run them from the benchmark directory in a Software Factory checkout. Set
`JOB_ID` to the actual Factory job and `C51_RUN_ID` to its matched C51 run; the
case statement fixes the issue ID and retained log path.

```sh
cd "$(git rev-parse --show-toplevel)/benchmarks/cleanrl-gpu"
: "${JOB_ID:?set JOB_ID to the running Factory job ID}"
: "${C51_RUN_ID:?set C51_RUN_ID to the matching manifest C51 run ID}"
: "${FACTORY_KUBERNETES_KUBECONFIG:?set the Factory Kubernetes kubeconfig path}"
export KUBECONFIG="$FACTORY_KUBERNETES_KUBECONFIG"
case "$C51_RUN_ID" in
  cleanrl-c51-gb10)
    ISSUE_RUN_ID=cleanrl-488-factory
    RL_LOG_PATH=.factory/benchmark/c51-gb10.log
    ;;
  cleanrl-c51-a100)
    ISSUE_RUN_ID=cleanrl-562-factory
    RL_LOG_PATH=.factory/benchmark/c51-a100.log
    ;;
  *) printf 'unknown C51 run ID: %s\n' "$C51_RUN_ID" >&2; exit 1 ;;
esac
ISSUE_OUT="data/$ISSUE_RUN_ID"
C51_OUT="data/$C51_RUN_ID"
mkdir -p "$ISSUE_OUT" "$C51_OUT"

PROFILE_SHOW=$(factory configure --show)
printf '%s\n' "$PROFILE_SHOW" |
  python3 scripts/collect.py profile --manifest run-manifest.json \
  --run-id "$ISSUE_RUN_ID" --output "$ISSUE_OUT/profile.json"
printf '%s\n' "$PROFILE_SHOW" |
  python3 scripts/collect.py profile --manifest run-manifest.json \
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
  pgrep -f '[c]leanrl/c51.py.*--total-timesteps 500000' >/dev/null 2>&1
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
  python3 scripts/collect.py kubernetes --manifest run-manifest.json \
  --run-id "$C51_RUN_ID" --output "$C51_OUT/kubernetes.json"

set -o pipefail
{ timeout 120s kubectl --namespace software-factory-execution exec "$POD_NAME" -- \
    sh -lc "while pgrep -f '[c]leanrl/c51.py.*--total-timesteps 500000' >/dev/null; do nvidia-smi --query-gpu=index,name,utilization.gpu,memory.used,power.draw --format=csv,noheader,nounits; sleep 1; done" ||
    [ "$?" -eq 124 ]; } |
  python3 scripts/collect.py gpu --manifest run-manifest.json \
  --run-id "$C51_RUN_ID" --interval-seconds 1 --output "$C51_OUT/gpu.json"
```

The Kubernetes collector requires the manifest node, RuntimeClass, image,
image ID, and one-GPU request/limit. The GPU collector requires the exact name
(`NVIDIA GB10` or `NVIDIA A100-SXM4-40GB`) and two or more in-window samples.

### Phase B: only after terminal success

Attach until terminal. Whole-job Factory/model metrics use the issue ID; only
C51 hardware/RL metrics use the matched C51 ID.

```sh
factory attach "$JOB_ID"

factory status "$JOB_ID" --json |
  python3 scripts/collect.py factory --manifest run-manifest.json \
  --run-id "$ISSUE_RUN_ID" --output "$ISSUE_OUT/factory.json"

JOB_STATE=$(factory status "$JOB_ID" --json |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["job"]["job"]["state"])')
if [ "$JOB_STATE" != succeeded ]; then
  printf 'benchmark job did not succeed: %s\n' "$JOB_STATE" >&2
  exit 1
fi

python3 scripts/collect.py model --manifest run-manifest.json \
  --run-id "$ISSUE_RUN_ID" \
  --factoryd-url "${FACTORYD_URL:-http://127.0.0.1:8787}" \
  --job-id "$JOB_ID" --page-limit 1000 --output "$ISSUE_OUT/model.json"

: "${FACTORY_KUBERNETES_WORKSPACE_HOST_DIR:?set the verified shared workspace host path}"
RL_LOG="$FACTORY_KUBERNETES_WORKSPACE_HOST_DIR/jobs/$JOB_ID/$RL_LOG_PATH"
test -f "$RL_LOG"
python3 scripts/collect.py rl --manifest run-manifest.json --run-id "$C51_RUN_ID" \
  --input "$RL_LOG" --output "$C51_OUT/rl.json"
```

After both jobs, merge all fourteen observations into four scoped rows:

```sh
python3 scripts/collect.py merge data/cleanrl-488-factory/*.json \
  data/cleanrl-562-factory/*.json data/cleanrl-c51-gb10/*.json \
  data/cleanrl-c51-a100/*.json --manifest run-manifest.json \
  --output data/metrics.csv
```

The profile collector proves Z.AI, GLM-5.2, the coding endpoint, and configured
key state without retaining a key. Factory/model collection independently
checks the job's immutable provider/model. Raw prompts, reasoning, Pod names,
paths, failures, and payload bodies are discarded.

The managed workspace root is `/workspaces/jobs/JOB_ID`. For the verified
shared local/NFS profile, the launcher maps it to
`$FACTORY_KUBERNETES_WORKSPACE_HOST_DIR/jobs/JOB_ID`; collect the named log
there before workspace cleanup. `factory export` exports only the Git patch,
not arbitrary `.factory` logs. Other existing-PVC profiles need an explicit
PVC reader and are not covered by the host-path command above.

The RL collector rejects truncated output: it requires raw `global_step`
records, the exit-zero `factory_training_steps=500000` receipt, and exactly
upstream `eval_episode` indices 0 through 9. PPO training returns remain
issue-execution evidence and must not be relabeled as evaluation.

## Verify and render

Collection, validation, rendering, and tests use only the Python standard
library:

```sh
cd "$(git rev-parse --show-toplevel)"
python3 -m unittest discover -s benchmarks/cleanrl-gpu/tests -v
```

`data/metrics.csv` intentionally has only its header. The renderer refuses an
empty dataset, so PNG/SVG files are created only after real normalized rows
exist:

```sh
python3 benchmarks/cleanrl-gpu/scripts/render_charts.py
```

One matched seed is an end-to-end functional comparison, not a statistical
performance claim. Such claims require repeated matched seeds.
