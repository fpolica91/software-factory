# DeepSeek

DeepSeek is an optional provider adapter, not Software Factory's default.
Configure it interactively:

```sh
factory configure
```

Select **DeepSeek**, enter the API key when prompted, and choose a model. Key
input is hidden and the resulting configuration is saved in the product
checkout's ignored `.env` file.

For noninteractive setup, provide the same values explicitly:

```sh
DEEPSEEK_API_KEY="..." factory configure \
  --provider deepseek \
  --model deepseek-v4-pro
```

The Rust provider adapter accepts native Codex Responses traffic and translates
it to DeepSeek's OpenAI-compatible Chat Completions API. The selected model and
endpoint are recorded as `FACTORY_MODEL` and `FACTORY_DEEPSEEK_BASE_URL`; normal
users should change them with `factory model` and `factory provider` rather
than editing `.env`.

Run `factory configure --show` to inspect the active provider without exposing
the key, `factory model list` to see the built-in choices, or pass another model
ID explicitly when the provider supports it.
