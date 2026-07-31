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
./factory install
cd /path/to/your/project
factory run "Review this codebase and explain its architecture"
```

Installation creates a symlink in `~/.local/bin`; add that directory to `PATH`
if the command reports it is missing. After that, `factory run` is the only
command needed: it creates local configuration, builds the image when needed,
starts the durable services, mounts the current Git repository, submits the
task, and attaches to it. On first use it asks for the configured provider key;
automation can pass `ZAI_API_KEY` in the environment instead.

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

The local CLI configures the pinned Z.AI profile on first use. For an unattended
first run, export `ZAI_API_KEY` or put it in the ignored `.env` file. The full
provider settings are:

```dotenv
ZAI_API_KEY=...
FACTORY_PROVIDER_AUTH_TOKEN=replace-this-local-default
FACTORY_MODEL=glm-5.2
FACTORY_MODEL_PROVIDER=factory-provider
FACTORY_PROVIDER_BASE_URL=http://zai-provider:10101/v1
FACTORY_MODEL_CATALOG_JSON=/var/lib/software-factory/provider/codex-models.json
```

The bridge fixes the model to exact `glm-5.2` and defaults to the Coding Plan
endpoint. Set `FACTORY_ZAI_BASE_URL=https://api.z.ai/api/paas/v4/` to select the
standard API. `ZAI_API_KEY` is read by the provider service.
`FACTORY_PROVIDER_AUTH_TOKEN` is the adapter's required internal API key used
by workflow runtimes to call the bridge.

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
| `zai` | provider bridge | Exact GLM-5.2 Coding Plan or standard API |
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
