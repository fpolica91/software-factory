# CleanRL #562: EnvPool dummy-step GAE

Work only from CleanRL commit
`fe8d8a03c41a7ef5b523e2e354bd01c363e786bb`. Implement the smallest correct
fix for #562 in the PyTorch `cleanrl/ppo_atari_envpool.py` path: EnvPool's
post-terminal dummy transition must not leak into GAE or train the terminal
observation with an invalid advantage. Add a focused regression at
`tests/test_ppo_atari_envpool_dummy_step.py`; avoid unrelated refactors. Do not
push upstream or open a pull request.

Keep raw command output under `.factory/benchmark/`. Verify the pinned
repository `uv.lock` SHA-256 is
`34ecd77065f7f99fabac27a8e562ba4894142e67774cb98d120ab527bb44df5b`, install
Python 3.10.16, and run exactly
`UV_PYTHON=3.10.16 uv sync --frozen --extra envpool --extra pytest --no-dev`.
Save dependency setup output as `.factory/benchmark/setup-a100.log`.

Run each gate and redirect stdout/stderr to the named file:

1. A CUDA preflight that selects device 0, moves a `torch.nn.Linear` model and
   tensor to CUDA, runs backward plus `optimizer.step()`, and asserts a model
   parameter changed: `.factory/benchmark/cuda-a100.log`.
2. `.venv/bin/python -m pytest -q
   tests/test_ppo_atari_envpool_dummy_step.py`:
   `.factory/benchmark/issue-562-pytest.log`.
3. `.venv/bin/python cleanrl/ppo_atari_envpool.py --env-id Breakout-v5
   --seed 1 --total-timesteps 10000000`:
   `.factory/benchmark/issue-562-ppo.log`.
4. Run `{ .venv/bin/python cleanrl/c51.py --env-id CartPole-v1 --seed 1 --total-timesteps 500000 --save-model && printf 'factory_training_steps=500000\n'; } > .factory/benchmark/c51-a100.log 2>&1`. The receipt must appear only after successful train, save, and ten-episode evaluation.

Fix failures and rerun the affected gate. Finish with a compact summary of
changed files, gate status, key final `global_step`/`SPS`/`eval_episode`
values, and log paths. Do not print raw logs.
