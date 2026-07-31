# Z.AI GLM 5.2

Software Factory routes Codex Responses traffic through the Factory provider
bridge and selects the OpenAI-compatible Z.AI Chat API. Use the endpoint that
matches the API-key plan:

| Profile | Base URL |
| --- | --- |
| Coding Developer Plan (default) | `https://api.z.ai/api/coding/paas/v4` |
| Standard applications and SDKs | `https://api.z.ai/api/paas/v4/` |

Both profiles use the exact model identifier `glm-5.2`. The documented key
variable is `ZAI_API_KEY`; `FACTORY_ZAI_API_KEY_ENV` may point the bridge at a
different variable when required by a host. See Z.AI's
[HTTP endpoint guide](https://docs.z.ai/guides/develop/http/introduction) and
[OpenAI Python SDK guide](https://docs.z.ai/guides/develop/openai/python).

The equivalent direct OpenAI Python SDK setup is:

```python
import os
from openai import OpenAI

client = OpenAI(
    api_key=os.environ["ZAI_API_KEY"],
    base_url="https://api.z.ai/api/coding/paas/v4",
)

response = client.chat.completions.create(
    model="glm-5.2",
    messages=[{"role": "user", "content": "Describe this repository."}],
)
```

This Python example documents provider compatibility; it is not another
Factory runtime. The shipped integration keeps the complete Codex harness and
uses the pinned provider bridge under
`factory-harness/factory/providers/bridge/`.

After starting that bridge, run the complete harness acceptance:

```sh
cd harness-client
FACTORY_PROVIDER_BASE_URL=http://127.0.0.1:18101/v1 npm run smoke:glm
```

That flow verifies a real GLM tool call, Codex compaction, runtime restart,
thread resume, and a second tool-using turn. `npm run smoke:glm:plan` separately
proves native plan mode and persisted plan context.
