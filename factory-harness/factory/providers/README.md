# Factory Provider Bridge

This crate contains optional translation adapters between the Codex Responses
API and providers that expose another wire protocol. Direct
Responses-compatible providers do not use this crate. Codex core remains
unchanged and continues to use `wire_api = "responses"`.

The optional profiles are Z.AI GLM 5.2 through its Standard API endpoint and
DeepSeek through its official OpenAI-compatible Chat endpoint.
Protocol translation is delegated to the maintained MIT-licensed
[`@bitkyc08/opencodex`](https://github.com/lidge-jun/opencodex) package, pinned
to version 2.8.0. Factory starts its public server API in an isolated state
directory. It does not run the `ocx` CLI, modify a user's Codex configuration,
or enable unrelated provider adapters.

Install the pinned bridge dependency once:

```sh
cd factory-harness/factory/providers/bridge
npm ci
```

Set `ZAI_API_KEY`, then run the bridge from the Factory workspace:

```sh
cargo run -p factory-providers --bin factory-provider-bridge
```

Packaged installations resolve bridge assets next to the executable at
`../lib/software-factory/provider-bridge`; source-tree resolution is only a
development fallback. Set `FACTORY_PROVIDER_RESOURCE_DIR` for another installed
layout. A container-facing deployment can bind and advertise different URLs:

```sh
FACTORY_PROVIDER_BIND_HOST=0.0.0.0 \
FACTORY_PROVIDER_ADVERTISED_URL=http://zai-provider:10101/v1 \
FACTORY_PROVIDER_STATE_DIR=/var/lib/software-factory/provider \
FACTORY_PROVIDER_BRIDGE_TOKEN=replace-this-local-token \
factory-provider-bridge
```

The advertised URL is the full Responses API base used in Codex thread config.
Non-loopback binds require `FACTORY_PROVIDER_BRIDGE_TOKEN`; Codex reads it
through the generated `X-OpenCodex-API-Key` environment-header mapping. This
adapter-local token is separate from `ZAI_API_KEY` and is not forwarded
upstream. `factory configure --preset zai` generates it automatically.
For Z.AI, the state directory contains `codex-models.json`; mount it into
workers at the same absolute path advertised by the deployment. DeepSeek uses
Codex fallback model metadata and does not require this catalog.

For GLM 5.2, Factory writes a typed Codex `ModelsResponse` catalog into the
isolated provider state directory. `ProviderBridge::selection()` returns the
model, provider ID, optional `model_catalog_json` path, and per-thread
configuration needed by `thread/start`. OpenCodex's Chat adapter preserves
Codex tool definitions and tool results without modifying Codex core or user
configuration.

Both official Z.AI OpenAI-compatible endpoint choices are explicit:

- Standard API (default within the `zai` preset):
  `https://api.z.ai/api/paas/v4`
- Coding Developer Plan: `https://api.z.ai/api/coding/paas/v4`

Both use model `glm-5.2`. Select Coding Developer Plan without changing code:

```sh
FACTORY_ZAI_BASE_URL=https://api.z.ai/api/coding/paas/v4 \
  cargo run -p factory-providers --bin factory-provider-bridge
```

`FACTORY_ZAI_API_KEY_ENV` remains a compatibility override for the name of the
key-bearing environment variable. The profile intentionally fixes the model to
exact `glm-5.2`.
`BridgeOptions::glm()` and `BridgeOptions::standard()` use the Standard API by
default; Rust callers can set `upstream_base_url` for Coding Plan. The bridge
binary accepts neutral `FACTORY_PROVIDER_UPSTREAM_ID`,
`FACTORY_PROVIDER_UPSTREAM_MODEL`, `FACTORY_PROVIDER_UPSTREAM_BASE_URL`, and
`FACTORY_PROVIDER_UPSTREAM_API_KEY_ENV` inputs. It prints the complete
`threadStart` fragment, including `config.model_catalog_json` when present, for
non-Rust callers.

The `deepseek` preset supplies provider ID `deepseek`, model
`deepseek-v4-pro`, base `https://api.deepseek.com`, and key environment variable
`DEEPSEEK_API_KEY`. It uses a separate state volume from Z.AI and relies on the
installed OpenCodex DeepSeek registry plus Codex fallback metadata.

The direct GLM functional acceptance lives in
`../../../harness-client/scripts/glm-tool-smoke.mjs`. It proves hidden-value tool
use, tool-result continuation, model-backed Codex compaction, a fresh runtime
process, persisted thread resume, and a second tool-using turn.
