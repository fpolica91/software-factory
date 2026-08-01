# DeepSeek

DeepSeek is an optional translation adapter, not Software Factory's default
provider. Configure it explicitly:

```sh
./factory configure --preset deepseek
```

The preset prompts for `DEEPSEEK_API_KEY`, generates an internal bridge token,
and saves these provider choices:

- official OpenAI-compatible Chat base: `https://api.deepseek.com`
- current preset model: `deepseek-v4-pro`
- internal Responses base: `http://deepseek-provider:10101/v1`

`deepseek-chat` was retired on 2026-07-24 and is not used by the preset. Check
DeepSeek's official [model list](https://api-docs.deepseek.com/api/list-models)
before overriding the neutral `FACTORY_MODEL` setting in `.env`.

The `deepseek` Compose profile starts the same pinned OpenCodex bridge used by
the Z.AI profile, configured with OpenCodex's built-in `deepseek` provider. It
translates Codex Responses requests to Chat Completions and translates streamed
tool calls and tool results back into the Codex protocol. DeepSeek uses its own
Compose state volume and host port (`DEEPSEEK_PROVIDER_PORT`, default `10102`),
so its state cannot collide with Z.AI's adapter state.

No downstream Codex model catalog is generated for this profile. Codex uses its
bounded fallback model metadata, while OpenCodex supplies its installed
DeepSeek-specific context, reasoning-history, and text-input conventions.
