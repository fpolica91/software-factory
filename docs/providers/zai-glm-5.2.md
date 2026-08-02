# Z.AI GLM 5.2

Z.AI is an optional provider adapter, not Software Factory's default. Configure
it interactively:

```sh
factory configure
```

Select **Z.AI**, enter the API key at the hidden prompt, select the Coding
Developer Plan or Standard API endpoint, then choose `glm-5.2`. The key and
selection are saved in the product checkout's ignored `.env` file.

The equivalent noninteractive commands are:

```sh
ZAI_API_KEY="..." factory configure \
  --provider zai \
  --base coding \
  --model glm-5.2

# Use --base standard for the Standard API endpoint.
```

The endpoint choices are:

| Choice | Base URL |
| --- | --- |
| `coding` | `https://api.z.ai/api/coding/paas/v4` |
| `standard` | `https://api.z.ai/api/paas/v4` |

The native Rust adapter translates Codex Responses traffic to Z.AI's
OpenAI-compatible Chat Completions API; it does not replace the Codex harness.
Use `factory configure --show` to inspect the active selection without exposing
the key, `factory provider zai` to switch back to Z.AI, and `factory model
glm-5.2` to select the model directly.
