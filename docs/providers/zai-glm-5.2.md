# Z.AI GLM 5.2

Z.AI GLM 5.2 is an optional translation adapter, not Software Factory's default
provider. Select it explicitly from the product checkout, then run a task from
the target Git repository:

```sh
./factory configure --preset zai
factory run "Review this codebase"
```

The preset prompts for `ZAI_API_KEY`, configures the bridge, and fixes the model
to `glm-5.2`. The bridge translates native Codex Responses traffic to Z.AI's
OpenAI-compatible Chat API. Use the upstream endpoint matching the API-key
plan:

| Profile | Base URL |
| --- | --- |
| Standard applications and SDKs (default) | `https://api.z.ai/api/paas/v4` |
| Coding Developer Plan | `https://api.z.ai/api/coding/paas/v4` |

Both profiles use the exact model identifier `glm-5.2`. The documented key
variable is `ZAI_API_KEY`; `FACTORY_ZAI_API_KEY_ENV` may point the bridge at a
different variable for an advanced manual deployment. See Z.AI's
[HTTP endpoint guide](https://docs.z.ai/guides/develop/http/introduction) and
[OpenAI Python SDK guide](https://docs.z.ai/guides/develop/openai/python).

Select Coding Developer Plan explicitly when that is the plan attached to the
key:

```sh
FACTORY_ZAI_BASE_URL=https://api.z.ai/api/coding/paas/v4 factory configure --preset zai
```

The equivalent direct OpenAI Python SDK setup is:

```python
import os
from openai import OpenAI

client = OpenAI(
    api_key=os.environ["ZAI_API_KEY"],
    base_url="https://api.z.ai/api/paas/v4",
)

response = client.chat.completions.create(
    model="glm-5.2",
    messages=[{"role": "user", "content": "Describe this repository."}],
)
```

This Python example documents the upstream API; it is not another Factory
runtime. The shipped integration keeps the complete Codex harness and uses the
pinned provider bridge under
`factory-harness/factory/providers/bridge/`.

After starting that bridge, run the complete harness acceptance:

```sh
cd harness-client
FACTORY_PROVIDER_BASE_URL=http://127.0.0.1:10101/v1 npm run smoke:glm
```

That flow verifies a real GLM tool call, Codex compaction, runtime restart,
thread resume, and a second tool-using turn. `npm run smoke:glm:plan` separately
proves native plan mode and persisted plan context.
