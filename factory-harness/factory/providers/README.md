# Factory Providers

This crate is Factory's provider boundary. Codex continues to speak its native
Responses API. OpenAI already supports that protocol and is configured directly;
it does not start this adapter. Anthropic, DeepSeek, and Z.AI use the native Rust
`factory-provider-bridge` binary to translate their official streaming APIs into
Responses events.

There is no JavaScript bridge, npm install, provider SDK, or mirrored Factory
protocol. The adapter exposes only:

- `GET /healthz`
- `POST /v1/responses` with `stream: true`

## Provider profiles

| ID | Transport | Default model | Key variable | Base URL |
| --- | --- | --- | --- | --- |
| `openai` | direct Responses | `gpt-5.6-sol` | `OPENAI_API_KEY` | `https://api.openai.com/v1` |
| `anthropic` | Messages adapter | `claude-sonnet-5` | `ANTHROPIC_API_KEY` | `https://api.anthropic.com` |
| `deepseek` | Chat adapter | `deepseek-v4-pro` | `DEEPSEEK_API_KEY` | `https://api.deepseek.com` |
| `zai` | Chat adapter | `glm-5.2` | `ZAI_API_KEY` | Coding or Standard API |

Z.AI's Coding Developer Plan base is
`https://api.z.ai/api/coding/paas/v4`; its Standard API base is
`https://api.z.ai/api/paas/v4`. `provider_profiles()` is the canonical catalog
used by onboarding and runtime configuration.

## Run an adapter

```sh
ZAI_API_KEY=... \
FACTORY_PROVIDER_UPSTREAM_ID=zai \
FACTORY_PROVIDER_UPSTREAM_MODEL=glm-5.2 \
FACTORY_PROVIDER_UPSTREAM_BASE_URL=https://api.z.ai/api/coding/paas/v4 \
cargo run -p factory-providers --bin factory-provider-bridge
```

The server binds to `127.0.0.1:10101` by default. Deployment may set
`FACTORY_PROVIDER_BIND_HOST`, `FACTORY_PROVIDER_PORT`,
`FACTORY_PROVIDER_ADVERTISED_URL`, and `FACTORY_PROVIDER_STATE_DIR`.
`FACTORY_PROVIDER_UPSTREAM_API_KEY_ENV` changes only the environment-variable
name from which the adapter reads the upstream key.
Run `factory-provider-bridge --help` for the complete environment contract;
help does not require an API key.

## Translation contract

The adapter preserves function, namespace, custom/freeform `apply_patch`, tool
results, parallel calls, streaming text, usage, and upstream error bodies.
Provider reasoning state is returned in the Responses reasoning item and replayed
on the next tool turn. That includes Z.AI/DeepSeek `reasoning_content` and
Anthropic's exact signed thinking blocks. Forked or resumed history also replays
completed calls whose tools are intentionally hidden from the current turn,
while live provider output must still resolve to a currently advertised tool.

Fixture tests need no API key:

```sh
cargo test -p factory-providers
cargo check -p factory-providers --no-default-features --lib
```
