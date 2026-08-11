# CleanRL #562: EnvPool dummy-step GAE

At commit `fe8d8a03c41a7ef5b523e2e354bd01c363e786bb`, fix #562 in
`cleanrl/ppo_atari_envpool.py`: prevent EnvPool's post-terminal dummy step from
leaking into GAE or invalid-advantage training. Add
`tests/test_ppo_atari_envpool_dummy_step.py`; avoid unrelated changes. Do not push upstream or open a pull request.

The mask `not (transition_done or current_dummy)` must gate bootstrapping and
lambda recurrence. Test the production GAE helper at mid-rollout and final-slot
dummy cases.

Store output under `.factory/benchmark/`. Verify `uv.lock` SHA-256:
`34ecd77065f7f99fabac27a8e562ba4894142e67774cb98d120ab527bb44df5b`.
`UV_CACHE_DIR=/tmp/factory-uv-cache uv python install 3.10.16`, then
`UV_CACHE_DIR=/tmp/factory-uv-cache UV_PYTHON=3.10.16
UV_PROJECT_ENVIRONMENT=/tmp/factory-cleanrl-venv uv sync --frozen --extra
envpool --extra pytest --no-dev`, then
`ln -sfn /tmp/factory-cleanrl-venv .venv`.
Run `mkdir -p /tmp/factory-runs && ln -sfn /tmp/factory-runs runs`.
Save setup output to `.factory/benchmark/setup-a100.log`.

Run gates sequentially; wait for exit and never overlap workloads:

1. Manifest CUDA preflight: real device-0 `torch.nn.Linear` backward plus
   `optimizer.step()` must change weight/bias. The log
   `.factory/benchmark/cuda-a100.log` must end exactly:
   `GPU name: NVIDIA A100-SXM4-40GB`; `weight changed: True`;
   `bias changed: True`; `CUDA PREFLIGHT PASS`.
2. `.venv/bin/python -m pytest -q
   tests/test_ppo_atari_envpool_dummy_step.py`:
   `.factory/benchmark/issue-562-pytest.log`.
3. `.venv/bin/python cleanrl/ppo_atari_envpool.py --env-id Breakout-v5
   --seed 1 --total-timesteps 10000000`:
   `.factory/benchmark/issue-562-ppo.log`. Only after exit 0, append
   `factory_ppo_training_steps=10000000` to that log.
4. Run `PYTHONPATH=$PWD .venv/bin/python cleanrl/c51.py --env-id CartPole-v1 --seed 1
   --total-timesteps 500000 --save-model` to `.factory/benchmark/c51-a100.log`.
   After exit 0 and ten `eval_episode` lines, copy its checkpoint to
   `.factory/benchmark/c51-a100.cleanrl_model`; only then append
   `factory_training_steps=500000`.

Fix failures. Summarize changes and final `global_step`/`SPS`/`eval_episode`.
Do not print, tee, or cat raw logs.
