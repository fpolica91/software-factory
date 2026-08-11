# Installation and Operations

This guide covers CLI installation, onboarding, Docker operation, and optional Kubernetes execution.

## Supported Runtime Scope

The host launcher requires a Unix-like Bash environment. Published Factory containers
are Linux images for `amd64` and `arm64`. Kubernetes execution requires Linux.

Normal use requires:

- Bash and Git with a Git repository to work on.
- A running Docker daemon and the Docker Compose plugin.
- One of `shasum`, `sha256sum`, or `openssl` for repository identity hashing.

The repository declares no minimum Docker, Compose, Git, Kubernetes, RAM, or
disk versions or sizes. Do not invent minimums from development machines.
Normal use needs no host Rust, Node.js, PostgreSQL, or Qdrant installation.

## Install the Launcher

Clone and retain the Factory checkout:

```sh
git clone https://github.com/fpolica91/software-factory.git
cd software-factory
./factory install
```

The installer creates `~/.local/bin/factory` as a symlink to this checkout's
`factory` script. It does not copy a standalone executable, so moving or
deleting the checkout breaks the command. Add the directory to `PATH` if needed:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Choose another directory with `FACTORY_INSTALL_DIR`:

```sh
FACTORY_INSTALL_DIR="$HOME/bin" ./factory install
export PATH="$HOME/bin:$PATH"
```

The installer will not overwrite an existing file or unrelated symlink.

## First Run and Interactive Onboarding

Run Factory from the Git checkout it should work on:

```sh
cd /path/to/project
factory
```

The first interactive run asks for provider, model, API key, any provider
endpoint choice, and task. API-key input is hidden, so typing or pasting shows
no characters. A successful read prints `API key received.`.

Factory creates `.env` in the retained Factory checkout, not the target
project. It is Git-ignored plaintext with mode `0600`; it is not encrypted.
Provider keys remain there for later switching. `factory configure --show`
reports only whether the selected key is configured.

| Provider | Credential variable | Default model |
| --- | --- | --- |
| OpenAI | `OPENAI_API_KEY` | `gpt-5.6-sol` |
| Anthropic | `ANTHROPIC_API_KEY` | `claude-sonnet-5` |
| DeepSeek | `DEEPSEEK_API_KEY` | `deepseek-v4-pro` |
| Z.AI | `ZAI_API_KEY` | `glm-5.2` |

`claude` is an input alias for `anthropic`. Guided selectors list known models
and accept a custom ID. Z.AI offers `coding` and `standard` endpoints.

```sh
factory configure
factory configure --show
factory provider list
factory provider show
factory provider anthropic
factory model list
factory model claude-sonnet-5
```

For non-interactive setup, load the provider key through the shell or a secret
manager, then pass it through the environment, not an `--api-key` argument:

```sh
env OPENAI_API_KEY="$OPENAI_API_KEY" factory configure --provider openai --model gpt-5.6-sol
env ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" factory configure --provider anthropic --model claude-sonnet-5
env DEEPSEEK_API_KEY="$DEEPSEEK_API_KEY" factory configure --provider deepseek --model deepseek-v4-pro
env ZAI_API_KEY="$ZAI_API_KEY" factory configure --provider zai --model glm-5.2 --base coding
```

Configuration persists the key, provider, endpoint, and model in `.env`.
Provider and model changes are checked against active jobs. `--force` changes
defaults for new jobs only; it does not migrate or stop existing jobs.

## Run and Operate Jobs

```sh
factory run "Implement authentication"
factory run --detach "Audit this repository"
factory run --no-apply "Review error handling"
factory run --repository https://git.example.invalid/org/repo.git --detach "Audit it"
```

An attached successful local run applies its result to the invoking checkout
by default. `--no-apply` retains it for explicit apply or export. `Ctrl-C`
detaches without stopping the job.

```sh
factory attach JOB_ID
factory attach --verbose JOB_ID
factory status JOB_ID
factory result JOB_ID
factory artifacts JOB_ID
factory stop JOB_ID
factory apply JOB_ID
factory export JOB_ID -o result.patch
factory logs
```

Apply requires the matching clean checkout. Export never overwrites an existing
destination. Logs follow the coordinator, worker, and selected provider bridge.

## Docker Services

The baseline stack contains PostgreSQL for durable state, Qdrant for long-term
memory, `factoryd` for coordination and worktrees, and `factory-worker` for the
native Codex lifecycle. OpenAI uses the direct Responses path. Anthropic,
DeepSeek, or Z.AI starts only its selected Rust translation bridge.

Redis (`coordination`), MinIO (`artifacts`), Ollama (`local-models`), and
Langfuse with ClickHouse (`observability`) are optional Compose profiles and
are not required by the baseline lifecycle.

```sh
factory up
factory logs
factory down
```

`factory down` stops containers while preserving data volumes.

## Images, Updates, and Source Builds

Normal use defaults to mutable image
`ghcr.io/fpolica91/software-factory:edge`, published from `main`. Release
automation also publishes commit-SHA and version tags. A current `edge` digest
is intentionally absent because the tag changes.

There is no `factory update` command. Update the retained checkout, then start
the stack. `factory up` pulls the configured non-local image:

```sh
cd /path/to/software-factory
git pull --ff-only
factory up
```

Build the complete Linux image through Docker without a host Rust toolchain:

```sh
factory build
FACTORY_IMAGE=software-factory:local factory up
```

The build tag is `software-factory:local`. Kubernetes nodes instead require a
registry-reachable immutable digest.

## Data and Artifacts

Factory-owned state includes:

- `.env` and `.factory-execution-backend` in the Factory checkout.
- Target-project `.factory/prompts/` and `.factory/jobs/JOB_ID/` renderings.
- Compose volumes for PostgreSQL, Qdrant, worktrees, Codex state, provider
  state, and coordinator artifacts.
- The configured host workspace mount for Kubernetes execution.

The launcher excludes project-local `.factory` paths from Git. PostgreSQL
records are durable truth; local Markdown and JSON files are renderings.

## Optional Kubernetes Execution

Use Kubernetes only from a fresh Factory installation not already pinned to
Docker. It requires:

- Linux, an existing cluster, and host `kubectl` or `k3s`.
- An absolute, readable kubeconfig.
- A cluster-reachable image in immutable
  `registry/repository@sha256:<digest>` form.

Set `FACTORY_EXECUTION_ENVIRONMENT_BACKEND=kubernetes` and matching
`FACTORY_KUBERNETES_*` values in the Factory checkout's `.env`.

`FACTORY_KUBERNETES_WORKSPACE_MODE=local` is the single-node K3s profile. It
requires exactly one node and creates a node-bound local PV/PVC.

`FACTORY_KUBERNETES_WORKSPACE_MODE=existing-pvc` requires an existing Bound,
Filesystem, `ReadWriteMany` PVC and an explicit writable host mount backed by
the same shared storage. Factory validates both paths but cannot prove they
share one backing filesystem.

Kata is optional and operator-installed. The supplied profile targets ARM64
K3s with RuntimeClass `kata-qemu-runtime-rs` and requires Helm and kubectl. It
modifies the host container runtime. Review
[ADR 0003](docs/adr/0003-single-host-k3s-execution-profile.md) and the [pinned deployment values](deploy/k3s/kata-qemu-runtime-rs.values.yaml) first.

## Safe Uninstall

There is no `factory uninstall` command. Stop the stack without deleting data:

```sh
factory down
```

Then identify the retained checkout and remove only its launcher symlink:

```sh
factory_checkout=/absolute/path/to/software-factory
factory_link="${FACTORY_INSTALL_DIR:-$HOME/.local/bin}/factory"
if [ -L "$factory_link" ] && [ "$(readlink "$factory_link")" = "$factory_checkout/factory" ]; then
  unlink "$factory_link"
else
  printf 'Refusing to remove unexpected path: %s\n' "$factory_link" >&2
fi
```

Replace `factory_checkout` with the checkout's absolute path. The checkout and
Docker volumes remain. Retain them for recovery, or remove them separately only
after intentionally deciding all jobs, worktrees, memory, and artifacts are
unneeded.

## Troubleshooting

- Command not found: add the install directory to `PATH` and verify the symlink.
- Docker errors: confirm `docker info` and `docker compose version` succeed.
- Blank key entry: hidden input displays nothing; wait for
  `API key received.` or rerun `factory configure`.
- Inspect configuration with `factory configure --show` and activity with
  `factory logs` or `factory attach JOB_ID`.
- Backend mismatch: `.factory-execution-backend` is immutable. Do not edit it
  to migrate data. Use a fresh checkout, distinct `FACTORY_PROJECT_NAME`, and
  separate data for another backend.

## Further Reading

- [README](README.md)
- [DeepSeek provider notes](docs/providers/deepseek.md)
- [Z.AI provider notes](docs/providers/zai-glm-5.2.md)
- [Codex kernel boundary](docs/adr/0001-codex-kernel-factory-extension.md)
- [Remote execution environments](docs/adr/0002-codex-remote-execution-environments.md)
- [Kubernetes execution profile](docs/adr/0003-single-host-k3s-execution-profile.md)
- [Contributing](CONTRIBUTING.md)
