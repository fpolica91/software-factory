# Software Factory

Software Factory adds a durable job lifecycle around the native Codex agent
lifecycle. `factoryd` owns jobs, leases, checkpoints, correlations, thread state,
and per-job Git worktrees. The workflow worker resumes the same Codex thread
across plan, execute, review, and remediation stages; it does not reimplement the
model or tool loop.

One image contains `factory-runtime`, `factoryd`, the provider bridge, the
TypeScript harness client, and the Hatchet workflows. The baseline deployment is
only PostgreSQL, Hatchet, Qdrant, `factoryd`, and the workflow worker. The worker
connects the runtime memory extension to Qdrant by default; `.env` can override
`FACTORY_QDRANT_COLLECTION` and `FACTORY_MEMORY_NAMESPACE`.

## Quick Start

Requirements are Docker Engine with Compose and enough resources to build the
Rust harness. No host agent CLI, database, Node, or Rust installation is
required.

```sh
git clone https://github.com/fpolica91/software-factory.git
cd software-factory
./factory configure
./factory install
cd /path/to/your/project
factory run "Review this codebase and explain its architecture"
```

`./factory configure` asks for a provider ID, Responses API base URL, model,
authentication mode, and API key. Installation then creates a symlink in
`~/.local/bin`; add that directory to `PATH` if the command reports it is
missing. From any Git repository, `factory run` builds the image when needed,
starts the durable services, mounts that repository, submits the task, and
attaches to it. Running `factory run` without saved provider configuration
opens the same neutral configuration prompts when a terminal is available.

The CLI is interactive when attached but does not require an interactive shell:

```sh
factory run "Implement authentication"             # submit and attach
factory run --detach "Implement authentication"    # print the job ID and exit
factory status JOB_ID                              # one-shot status
factory attach JOB_ID                              # reconnect and handle approvals
factory stop JOB_ID                                # durably cancel the job
```

`Ctrl-C` detaches the terminal; it does not stop the job. A detached job keeps
running in Hatchet and `factoryd`, survives worker restarts, and can be attached
again later. When stdin is not a TTY, `run` automatically behaves as detached.
Only one direct-checkout job runs at a time; Factory refuses to remount a
different repository while that job is active.

## Model Providers

The direct path supports providers that expose an OpenAI-compatible Responses
API. It uses Codex's native provider configuration and agent loop; Software
Factory does not invoke the Claude Code or Cursor SDK/harness. Review saved
settings without exposing key values:

```sh
factory configure --show
```

For an unattended first run, export the neutral provider settings before
calling `factory run --detach`, or put them in the product checkout's ignored
`.env` file:

```dotenv
FACTORY_PROVIDER_ADAPTER=responses
FACTORY_MODEL_PROVIDER=configured-provider
FACTORY_PROVIDER_BASE_URL=https://provider.example/v1
FACTORY_MODEL=provider-model-id
FACTORY_PROVIDER_AUTH=key
FACTORY_PROVIDER_API_KEY=...
```

Set `FACTORY_PROVIDER_AUTH=none` and omit the key only for an endpoint that does
not require authentication. Providers using Chat, Anthropic, or another wire
protocol require an explicitly selected translation adapter; they are not
silently treated as Responses endpoints.

Z.AI GLM 5.2 is one optional translated profile. Select it explicitly:

```sh
factory configure --preset zai
factory run "Review this codebase"
```

The preset asks for `ZAI_API_KEY`, generates its internal bridge token, and
selects the Standard API URL. Coding Developer Plan keys can select their
endpoint explicitly in one line:

```sh
FACTORY_ZAI_BASE_URL=https://api.z.ai/api/coding/paas/v4 factory configure --preset zai
```

See [`docs/providers/zai-glm-5.2.md`](docs/providers/zai-glm-5.2.md) for
adapter details.

DeepSeek is a separate optional translated profile:

```sh
factory configure --preset deepseek
factory run "Review this codebase"
```

The preset asks for `DEEPSEEK_API_KEY`, selects the official
`https://api.deepseek.com` Chat endpoint, and persists `deepseek-v4-pro` as the
model. The shared bridge translates Chat Completions to Responses while
preserving Codex tool calls. See
[`docs/providers/deepseek.md`](docs/providers/deepseek.md) for adapter and model
override details.

## Durable Jobs and Workspaces

CLI jobs operate directly on the Git repository from which `factory` is called.
The same job keeps one native Codex thread across planning, execution, review,
and remediation, with durable checkpoints between stages. The coordinator also
supports remote repositories as managed worktrees under `/workspaces` for API
clients and future integrations.

## Integration Plugins

External trackers and source hosts are plugins, not core dependencies. Install
an ESM adapter in the image and configure it explicitly:

```dotenv
FACTORY_INTEGRATION_PLUGINS_JSON=[{"module":"file:///opt/factory-plugins/tracker/index.js","config":{}}]
```

Then associate a durable job with its external work item in job input:

```json
{"integration":{"intake":{"adapter":"tracker","externalId":"WORK-42"}}}
```

The worker publishes deterministic lifecycle event IDs inside the factoryd
attempt boundary. A failed delivery resumes from the completed Codex checkpoint
and retries the adapter without repeating the model stage. Adapter contracts
live in `integrations/`; no tracker or source host is enabled by default.

Model, provider, runtime, and Codex-home deployment defaults come from `.env`.
Job input overrides deployment defaults, and operation input is authoritative.

## Optional Profiles

| Profile | Services | Purpose |
|---|---|---|
| `zai` | provider bridge | Exact GLM-5.2 Standard API by default; Coding Plan by override |
| `deepseek` | provider bridge | DeepSeek Chat translation with `deepseek-v4-pro` by default |
| `coordination` | Redis | Optional coordination, cache, or pub/sub; never durable job state |
| `artifacts` | MinIO | Optional S3-compatible build and run artifacts |
| `local-models` | Ollama | Optional local model server |
| `observability` | Langfuse web/worker, ClickHouse, Redis, MinIO | Complete opt-in trace stack; also uses baseline PostgreSQL |

Enable one or more with `docker compose --profile <name> up -d`. Profile data
uses separate volumes, so observability Redis and MinIO are not silently reused
as Factory coordination or artifact stores.

## Operations and Source Layout

```sh
factory up                                    # start or repair the local stack
factory logs                                  # follow Factory service logs
factory build                                 # rebuild the shared image
factory down                                  # stop services; preserve job data
```

- `factory-harness/factory/` contains the Rust runtime, coordinator, protocol,
  extension seam, and provider bridge.
- `harness-client/` is the typed process/protocol client.
- `workflows/` contains the durable Hatchet task and direct runner.
- `integrations/` defines neutral intake, source-host, CI, and artifact adapter
  contracts; no concrete adapter is enabled by default.
- `postgres-init/` creates the baseline Hatchet database. The observability
  profile creates its Langfuse database on demand.

Factory disables Codex analytics and OpenTelemetry export in the image by
default. Hatchet analytics, Qdrant telemetry, and optional Langfuse telemetry
are also disabled through their deployment flags.
