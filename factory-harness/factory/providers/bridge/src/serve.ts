import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

const port = Number(argument('--port') ?? process.env.FACTORY_PROVIDER_PORT ?? '10101');
if (!Number.isInteger(port) || port < 1 || port > 65535) {
  throw new Error('provider bridge port must be an integer from 1 to 65535');
}
const hostname = argument('--host') ?? process.env.FACTORY_PROVIDER_BIND_HOST ?? '127.0.0.1';
if (!hostname) {
  throw new Error('provider bridge host must not be empty');
}

const stateDir = resolve(argument('--state-dir') ?? process.env.FACTORY_PROVIDER_STATE_DIR ?? '.state');
const model = 'glm-5.2';
const baseUrl = process.env.FACTORY_ZAI_BASE_URL ?? 'https://api.z.ai/api/coding/paas/v4';
const apiKeyEnv = process.env.FACTORY_ZAI_API_KEY_ENV ?? 'ZAI_API_KEY';
if (!/^[A-Z_][A-Z0-9_]*$/.test(apiKeyEnv)) {
  throw new Error('FACTORY_ZAI_API_KEY_ENV must name an environment variable');
}
if (!process.env[apiKeyEnv]) {
  throw new Error(`${apiKeyEnv} is required to start the Z.AI provider bridge`);
}
const admissionToken = process.env.FACTORY_PROVIDER_AUTH_TOKEN?.trim();
if (hostname !== '127.0.0.1' && hostname !== 'localhost' && !admissionToken) {
  throw new Error('FACTORY_PROVIDER_AUTH_TOKEN is required for a non-loopback provider bind');
}
if (admissionToken) {
  process.env.OPENCODEX_API_AUTH_TOKEN = admissionToken;
}

mkdirSync(stateDir, { recursive: true });
process.env.OPENCODEX_HOME = stateDir;

const config = {
  port,
  hostname,
  defaultProvider: 'factory-zai',
  providers: {
    'factory-zai': {
      adapter: 'openai-chat',
      baseUrl,
      authMode: 'key',
      apiKey: `\${${apiKeyEnv}}`,
      defaultModel: model,
      models: [model],
      selectedModels: [model],
      liveModels: false,
      modelContextWindows: { [model]: 1_000_000 },
      modelSuffixBracketStrip: true,
      noVisionModels: [model],
      modelReasoningEfforts: { [model]: ['low', 'medium', 'high', 'xhigh', 'max'] },
      preserveReasoningContentModels: [model]
    }
  },
  websockets: false,
  multiAgentGuidanceEnabled: false,
  subagentModels: [],
  codexAutoStart: false,
  codexShimAutoRestore: false,
  syncResumeHistory: false
};

writeFileSync(resolve(stateDir, 'config.json'), `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 });

const { startServer } = await import('@bitkyc08/opencodex');
const server = startServer(port);

const stop = () => {
  server.stop(true);
  process.exit(0);
};
process.on('SIGINT', stop);
process.on('SIGTERM', stop);
