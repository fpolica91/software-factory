# CleanRL GPU Benchmark Report

## Scope and provenance

- CleanRL revision: `fe8d8a03c41a7ef5b523e2e354bd01c363e786bb`
- Agent: Z.AI provider, `glm-5.2`, `coding` base/plan
- Issue gates: #488 on arm64 GB10; #562 on amd64 A100
- Matched train/eval runs: C51 CartPole on GB10 and A100
- Execution: RuntimeClass `nvidia`, one `nvidia.com/gpu`; `NVIDIA GB10` on
  `spark-91b3` and `NVIDIA A100-SXM4-40GB` on `kent-ai-stuff`
- Image provenance: node-local `software-factory-gpu:benchmark-2b88673` with
  the per-node content digests in `run-manifest.json`; public digest pending
- Dependencies: hashed Python 3.12 GB10 lock; pinned CleanRL `uv.lock` on A100
- Configuration: `run-manifest.json`
- Aggregate data: `data/metrics.csv`

Every future metrics row must retain `provider=zai`, `model=glm-5.2`, and
`provider_base=coding` from the strict configuration receipt. Job records must
independently report `zai`/`glm-5.2`; mismatches are invalid.

## Measured facts

No benchmark jobs have been run. The metrics file has zero data rows, and no
PNG or SVG chart is published. No correctness, runtime, model-usage,
Kubernetes, GPU, or RL result is currently available.

After execution, record only normalized aggregates: Factory state/duration and
operation counts; model response/token totals, tool starts, and compactions;
Pod phases and restarts; a bounded in-Pod GPU utilization, memory, and power
time series; and the C51 ten-episode evaluation return distribution.

The four rows have distinct scope: `cleanrl-488-factory` and
`cleanrl-562-factory` contain whole issue-job Factory/model metrics, while
`cleanrl-c51-gb10` and `cleanrl-c51-a100` contain only C51-synchronized
Kubernetes, GPU, and RL metrics. Do not compare unlike columns across scopes.

## Issue-correctness evidence

- #488 requires its focused ToyText regression and successful real
  `FrozenLake-v1` PPO training on GB10.
- #562 requires its focused PyTorch EnvPool dummy-step/GAE regression and
  successful real `Breakout-v5` PPO training on A100.

Record exact candidate revisions and outcomes only after execution. PPO
training episodic returns and SPS establish that training ran; the focused
regressions establish issue correctness.

## Product-validation evidence

The GB10 and A100 C51 runs use the same pinned CleanRL revision, environment,
seed, training steps, and ten-episode evaluator. Record their results only
after CUDA preflight, training, save, and evaluation all succeed.

## Conclusions

None yet. Do not infer correctness, durability, GPU effectiveness, or
comparative performance from the plan. One matched seed is a functional gate;
performance claims require repeated matched seeds.
