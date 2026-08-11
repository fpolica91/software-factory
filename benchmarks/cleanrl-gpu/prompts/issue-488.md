# CleanRL #488: ToyText support

Work only from CleanRL commit
`fe8d8a03c41a7ef5b523e2e354bd01c363e786bb`. Implement the smallest
fix for #488: support ToyText discrete observations such as `FrozenLake-v1` in
`cleanrl/ppo.py` without breaking Box-space PPO.
Add a focused regression at `tests/test_ppo_toytext.py`; avoid unrelated
refactors. Do not push upstream or open a pull request.

Write raw output under `.factory/benchmark/`. Download `gb10.in` and
`gb10.txt` into that directory from
`https://raw.githubusercontent.com/fpolica91/software-factory/main/benchmarks/cleanrl-gpu/requirements/`,
then verify their SHA-256 values are respectively
`eaf3fa41a93aad1b2ee80aed5fae1b36248dad8bf5ff8b151de6f12dd814b105`
and `11523ca83e7da13ef6aa7c5f45945d0e93f601f55d80348211504ccf1ac1bd47`.
Create `.venv` with `python3.12 -m venv --system-site-packages .venv`, then run
`uv pip install --python .venv/bin/python --require-hashes --no-deps -r
.factory/benchmark/gb10.txt`. Torch must remain the NVIDIA image's system CUDA
build. Save dependency setup output as `.factory/benchmark/setup-gb10.log`.

Run each gate and redirect stdout/stderr to the named file:

1. A CUDA preflight on device 0 that moves a `torch.nn.Linear` and tensor to
   CUDA, runs backward plus `optimizer.step()`, and proves a parameter changed:
   `.factory/benchmark/cuda-gb10.log`.
2. `.venv/bin/python -m pytest -q tests/test_ppo_toytext.py`:
   `.factory/benchmark/issue-488-pytest.log`.
3. `.venv/bin/python cleanrl/ppo.py --env-id FrozenLake-v1 --seed 1
   --total-timesteps 500000`: `.factory/benchmark/issue-488-ppo.log`.
4. Run `{ .venv/bin/python cleanrl/c51.py --env-id CartPole-v1 --seed 1 --total-timesteps 500000 --save-model && printf 'factory_training_steps=500000\n'; } > .factory/benchmark/c51-gb10.log 2>&1`. The receipt must appear only after successful train, save, and ten-episode evaluation.

Fix failures and rerun the affected gate. Finish with a compact summary of
changed files, gate status, key final `global_step`/`SPS`/`eval_episode`
values, and log paths. Do not print raw logs.
