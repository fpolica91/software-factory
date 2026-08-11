# CleanRL #488: ToyText support

Use CleanRL commit `fe8d8a03c41a7ef5b523e2e354bd01c363e786bb`. Fix #488
in `cleanrl/ppo.py`: support ToyText discrete observations (`FrozenLake-v1`)
without breaking Box PPO. Add `tests/test_ppo_toytext.py`; avoid unrelated
changes. Do not push upstream or open a pull request.

Store raw output under `.factory/benchmark/`. Fetch `gb10.in` and `gb10.txt`
there from
`https://raw.githubusercontent.com/fpolica91/software-factory/6e967bb67a92944ecdfa9b5884cc1924f2dbecba/benchmarks/cleanrl-gpu/requirements/`,
and verify SHA-256 values
`eaf3fa41a93aad1b2ee80aed5fae1b36248dad8bf5ff8b151de6f12dd814b105`
and `11523ca83e7da13ef6aa7c5f45945d0e93f601f55d80348211504ccf1ac1bd47`.
Run `python3.12 -m venv --system-site-packages --without-pip
/tmp/factory-cleanrl-venv`, `UV_LINK_MODE=copy uv pip install --python
/tmp/factory-cleanrl-venv/bin/python --require-hashes --no-deps -r
.factory/benchmark/gb10.txt`, then `ln -sfn /tmp/factory-cleanrl-venv .venv`.
Run `mkdir -p /tmp/factory-runs && ln -sfn /tmp/factory-runs runs`.
Keep the image's CUDA Torch. Save setup output as
`.factory/benchmark/setup-gb10.log`.

Run gates sequentially; wait for exit and never overlap workloads:

1. Manifest CUDA preflight: real device-0 `torch.nn.Linear` backward plus
   `optimizer.step()` must change weight and bias. End
   `.factory/benchmark/cuda-gb10.log` with exactly `GPU name: NVIDIA GB10`,
   `weight changed: True`, `bias changed: True`, then `CUDA PREFLIGHT PASS`.
2. `.venv/bin/python -m pytest -q tests/test_ppo_toytext.py`:
   `.factory/benchmark/issue-488-pytest.log`.
3. `.venv/bin/python cleanrl/ppo.py --env-id FrozenLake-v1 --seed 1
   --total-timesteps 500000`: `.factory/benchmark/issue-488-ppo.log`. Only
   after exit 0, append `factory_ppo_training_steps=500000` to that log.
4. Run `PYTHONPATH=$PWD .venv/bin/python cleanrl/c51.py --env-id CartPole-v1 --seed 1
   --total-timesteps 500000 --save-model` to `.factory/benchmark/c51-gb10.log`.
   After exit 0 and ten `eval_episode` lines, copy its checkpoint to
   `.factory/benchmark/c51-gb10.cleanrl_model`; only then append
   `factory_training_steps=500000`.

Fix failures. Summarize changes and final `global_step`/`SPS`/`eval_episode`.
Do not print, tee, or cat raw logs.
