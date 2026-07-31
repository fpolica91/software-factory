# Factory harness client

`@software-factory/harness-client` is the stable TypeScript boundary between
Software Factory and the native `factory-runtime` extension. Exact types from
the pinned Codex app-server V2 protocol are the active runtime and automation
lifecycle. Factory Protocol V1 wrappers remain available only as a compatibility
and incremental-migration surface. Codex remains the execution kernel; this
package does not contain another agent loop, tool harness, or provider.

## Build

```sh
npm ci
npm run sync:protocol
npm run build
```

Protocol artifacts are copied deterministically from `../factory-harness/factory/protocol/schema`. Files under `src/protocol/v1/generated/` are generated and must not be edited by hand. `npm run check:protocol` fails when the checked-in copy is stale.

The complete pinned Codex app-server TypeScript surface is copied separately
from `../factory-harness/codex-rs/app-server-protocol/schema/typescript`:

```sh
npm run sync:codex-v2
npm run check:codex-v2
```

Files under `src/codex-v2/generated/` are generated and must not be edited by
hand. The generated `CODEX_V2_PROTOCOL_MANIFEST` records the Codex revision,
the SHA-256 digest of the upstream V2 schema bundle, and the exact stable
client-request, notification, and server-request method sets. Client startup
exact-compares the runtime's Factory version/digest, pinned Codex revision, and
Codex V2 version/digest against the generated manifests before initialization.

## Use

```ts
import { FactoryClient, type FactoryCorrelationSeed } from '@software-factory/harness-client';

const correlation: FactoryCorrelationSeed = {
  jobId: 'job-17',
  operationId: 'operation-4',
  attemptId: 'attempt-1',
  workflowRunId: 'hatchet-run-9',
  taskRunExternalId: 'task-run-12',
};

const client = await FactoryClient.connect({
  runtimePath: '/opt/software-factory/bin/factory-runtime',
  cwd: '/work/repository',
  onCodexCorrelatedNotification: ({ notification, correlation }) => {
    console.log(notification.notification.method, correlation);
  },
  onCodexServerRequest: async ({ request }) => {
    throw new Error(`host did not handle ${request.method}`);
  },
});

const started = await client.requestCodexCorrelated('thread/start', {
  cwd: '/work/repository',
}, correlation);
const turn = await client.requestCodexCorrelated('turn/start', {
  threadId: started.response.thread.id,
  input: [{ type: 'text', text: 'Implement the requested change', text_elements: [] }],
}, correlation);

console.log(started.requestId, turn.requestId, turn.response.turn.id);

await client.close();
```

`connect()` first verifies the runtime distribution manifest against both the
generated Factory manifest and pinned Codex revision/schema digest, then
performs `initialize` followed by `initialized`. The legacy Factory-only
manifest is compatibility output and is never used for active negotiation.
`requestCodexCorrelated` selects the generated request params and known result
type, returns the exact wire request ID, and seeds durable correlation for exact
notifications. `requestCodex` and `requestRaw` remain available when correlation
is not required.

Exact server requests reach `onCodexServerRequest`. Factory V1 methods such as
`startThread`, `startTurn`, `onEvent`, and `onServerRequest` continue to decode
projected requests/events for compatibility, but are not the target workflow
path. Correlations retain job, operation, attempt, workflow run, task run,
request, thread, turn, and item identities.

Run the direct lifecycle smoke after building the Rust runtime and this package:

```sh
npm run smoke:manifest
npm run smoke
```

The focused manifest smoke proves a real runtime/client initialization succeeds,
then substitutes an ephemeral runtime manifest with only the Codex V2 digest
altered and proves the client rejects it before starting the runtime lifecycle.

## Pinned Codex V2 lane

Pinned Codex V2 is the automation contract used by Factory workflows. The
Factory crate does not reimplement upstream request and response types.
Planning uses `turn/start` with the experimental `collaborationMode` plan
payload; execution and remediation use ordinary `turn/start`; review uses
inline native `review/start` with a custom target. Every stage resumes the same
thread and waits on exact `turn/completed` notifications.

```ts
import { FactoryClient } from '@software-factory/harness-client';
import { CODEX_V2_PROTOCOL_MANIFEST } from '@software-factory/harness-client/codex-v2';
import type {
  ConfigReadResponse,
  ThreadListResponse,
} from '@software-factory/harness-client/codex-v2/v2';

const client = await FactoryClient.connect({
  runtimePath: '/opt/software-factory/bin/factory-runtime',
  onCodexNotification: ({ kind, notification }) => {
    if (kind === 'known') console.log(notification.method, notification.params);
  },
});

// The method selects its exact upstream params type.
const listed = await client.requestCodex('thread/list', {
  limit: 50,
  archived: false,
}) as ThreadListResponse;

// Raw requests are the forward-compatible path for experimental or newer
// methods and let the caller select the expected result type.
const config = await client.requestRaw<ConfigReadResponse>('config/read', {
  includeLayers: true,
});

console.log(CODEX_V2_PROTOCOL_MANIFEST.schemaSha256, listed.data, config.config);
```

`client.codexNotifications` yields every upstream notification as either a
generated `ServerNotification` (`kind: 'known'`) or an exact raw fallback.
`client.codexCorrelatedNotifications` yields the same stream with durable
Factory correlation when thread/turn/item identity is known.
`client.codexServerRequests` does the same for server-initiated requests. When
`onCodexServerRequest` is supplied, it handles generated upstream requests and
returns either `{result}` or `{error}`; it takes precedence over the projected
Factory V1 handler for that request. Without it, the existing Factory handler
and method-not-supported behavior remain unchanged.

The package exports the upstream shared types from
`@software-factory/harness-client/codex-v2` and the V2-specific types from
`@software-factory/harness-client/codex-v2/v2`, avoiding conflicting wildcard
exports in the main Factory namespace.

Run the functional V2 acceptance against the real runtime and model bridge:

```sh
FACTORY_PROVIDER_BASE_URL=http://127.0.0.1:18101/v1 \
FACTORY_MODEL_CATALOG_JSON=/tmp/software-factory-provider-glm52/codex-models.json \
npm run smoke:codex-v2
```

It exercises effective config and model discovery, materializes one real
thread, then proves list, read, raw read, archive, archived listing, unarchive,
restored listing, and exact archived/unarchived notifications.

Run the pinned Codex-kernel parity acceptance separately:

```sh
FACTORY_PROVIDER_BASE_URL=http://127.0.0.1:18101/v1 \
FACTORY_MODEL_CATALOG_JSON=/tmp/software-factory-provider-glm52/codex-models.json \
npm run smoke:kernel-parity
```

This creates a disposable Codex home, Git workspace, user skill, and local
stdio MCP server. Through the public V2 client lane it proves skill discovery;
MCP inventory, resource reads, tool calls, and live reload; goal set/get/clear
with exact notifications; an inline review of an uncommitted fixture change;
and native Codex MultiAgentV2 collaboration.
The collaboration receipt asserts the current MultiAgentV2 vocabulary directly
from persisted raw upstream function-call items: `list_agents`, `spawn_agent`,
`send_message`, `wait_agent`, `followup_task`, and `interrupt_agent`. It then
lists and reads the persisted child thread. GLM 5.2 is used only for the turns
that require model execution. The temporary tree is removed when the acceptance
finishes.

The smoke uses a temporary Codex home and exercises manifest negotiation, initialize/initialized, thread start, compact, resume, and clean EOF shutdown without starting a model turn.

Run the end-to-end GLM acceptance with the Factory provider bridge already
listening locally:

```sh
FACTORY_PROVIDER_BASE_URL=http://127.0.0.1:10101/v1 npm run smoke:glm
```

This is a functional acceptance flow, not a protocol unit test. GLM 5.2 must
drive Codex to create and inspect a file through separate shell-tool calls,
continue after both tool outputs, compact the thread, restart the runtime,
resume the persisted thread, and complete a second tool-using turn. The flow
uses Codex's standard `danger-full-access` sandbox mode because the acceptance
environment must permit the spawned command to run.

Run the plan-mode and restart/resume acceptance separately:

```sh
FACTORY_PROVIDER_BASE_URL=http://127.0.0.1:18101/v1 npm run smoke:glm:plan
```

This flow requires GLM 5.2 in Plan mode to publish a substantive completed
`plan` item and finish successfully without changing the fixture workspace.
It then restarts `factory-runtime`, finds that exact item in resumed history,
and asks the resumed model context to copy the plan into the normal-mode
`update_plan` checklist, which must emit `turnPlanUpdated`. These are separate
Codex capabilities: `update_plan` is intentionally unavailable in Plan mode.
The temporary Codex home registers `factory-provider` globally and uses
`never` approval with the direct-execution sandbox baseline used by these
isolated temporary acceptance workspaces; the script independently asserts
that the fixture tree remains byte-for-byte unchanged.

Run the native Factory state acceptance separately:

```sh
FACTORY_PROVIDER_BASE_URL=http://127.0.0.1:18101/v1 \
FACTORY_MODEL_CATALOG_JSON=/absolute/path/to/codex-models.json \
npm run smoke:glm:factory-state
```

GLM 5.2 must call the native decomposition, progress, review, and remediation
tools with exact structured inputs. The script checks every tool receipt, then
starts a second turn on the same thread and requires the model to return the
exact accumulated state injected through native Factory context. It makes no
workspace changes. `FACTORY_MODEL_CATALOG_JSON` is optional, but when supplied
it must name an existing absolute catalog path. The default extension backend
is process memory for this acceptance; it is not crash durability.

Run the durable factoryd restart acceptance with factoryd and the supervised
provider already listening:

```sh
FACTORYD_URL=http://127.0.0.1:8787/v1 \
FACTORY_PROVIDER_BASE_URL=http://127.0.0.1:18102/v1 \
FACTORY_MODEL_CATALOG_JSON=/tmp/software-factory-provider-glm52/codex-models.json \
npm run smoke:glm:factoryd-state
```

This dedicated flow verifies GLM's exact four native state mutations and the
opaque document stored by factoryd, stops the entire `factory-runtime`, starts
a fresh process, and resumes the same Codex thread. It then checks the exact
revision-4 state injected with `durability: "durable"` before requiring GLM to
return that state without mutation tools. The factoryd record must remain at
revision 4 and the fixture workspace must remain byte-for-byte unchanged.
