import assert from 'node:assert/strict';
import { access, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { FactoryClient } from '../dist/index.js';
import { CODEX_V2_PROTOCOL_MANIFEST } from '../dist/codex-v2/index.js';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const runtimePath = process.env.FACTORY_RUNTIME_PATH ?? resolve(
  packageRoot,
  '..',
  'factory-harness',
  'factory',
  'target',
  'debug',
  'factory-runtime',
);
const providerBaseUrl = process.env.FACTORY_PROVIDER_BASE_URL ?? 'http://127.0.0.1:18101/v1';
const modelCatalogJson = process.env.FACTORY_MODEL_CATALOG_JSON ??
  '/tmp/software-factory-provider-glm52/codex-models.json';
const model = process.env.FACTORY_ZAI_MODEL ?? 'glm-5.2';
const timeoutMs = Number(process.env.FACTORY_SMOKE_TIMEOUT_MS ?? '300000');

await access(runtimePath);
if (!isAbsolute(modelCatalogJson)) {
  throw new Error('FACTORY_MODEL_CATALOG_JSON must be an absolute path');
}
await access(modelCatalogJson);
if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
  throw new Error('FACTORY_SMOKE_TIMEOUT_MS must be a positive number');
}

const temporaryRoot = await mkdtemp(resolve(tmpdir(), 'factory-codex-v2-smoke-'));
const codexHome = resolve(temporaryRoot, 'codex-home');
const workspace = resolve(temporaryRoot, 'workspace');
await mkdir(codexHome, { recursive: true });
await mkdir(workspace, { recursive: true });
await writeFile(resolve(workspace, 'README.md'), '# Codex V2 functional acceptance\n');
await writeFile(resolve(codexHome, 'config.toml'), [
  `model = ${JSON.stringify(model)}`,
  'model_provider = "factory-provider"',
  'model_context_window = 1000000',
  'model_reasoning_effort = "low"',
  'approval_policy = "never"',
  'sandbox_mode = "danger-full-access"',
  `model_catalog_json = ${JSON.stringify(modelCatalogJson)}`,
  '',
  '[model_providers.factory-provider]',
  'name = "Software Factory provider bridge"',
  `base_url = ${JSON.stringify(providerBaseUrl)}`,
  'wire_api = "responses"',
  'requires_openai_auth = false',
  'supports_websockets = false',
  '',
].join('\n'), { mode: 0o600 });

const seed = {
  jobId: 'codex-v2-functional-acceptance',
  operationId: 'codex-v2-surface',
  attemptId: 'codex-v2-attempt-1',
};
const projectedEvents = [];
const codexNotifications = [];
const errors = [];
let client;

async function waitForProjected(predicate, description) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const match = projectedEvents.find(({ event }) => predicate(event));
    if (match) return match.event;
    if (errors.length > 0) throw errors.at(-1);
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 50));
  }
  throw new Error(`timed out waiting for ${description}`);
}

async function waitForCodexNotification(method) {
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    const match = codexNotifications.find((entry) =>
      entry.kind === 'known' && entry.notification.method === method,
    );
    if (match) return match.notification;
    if (errors.length > 0) throw errors.at(-1);
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 25));
  }
  throw new Error(`timed out waiting for exact Codex notification ${method}`);
}

try {
  client = await FactoryClient.connect({
    runtimePath,
    cwd: workspace,
    env: { CODEX_HOME: codexHome },
    onEvent: (event) => projectedEvents.push(event),
    onCodexNotification: (notification) => codexNotifications.push(notification),
    onError: (error) => errors.push(error),
  });
  assert.equal(
    client.manifest.sourceCodexRevision,
    CODEX_V2_PROTOCOL_MANIFEST.sourceCodexRevision,
    'runtime revision does not match synced Codex V2 types',
  );

  const config = await client.requestCodex('config/read', {
    includeLayers: true,
    cwd: workspace,
  });
  assert.equal(config.config.model, model, 'config/read lost the effective model');
  assert.ok(Array.isArray(config.layers), 'config/read did not include config layers');

  const models = await client.requestCodex('model/list', {
    limit: 100,
    includeHidden: true,
  });
  assert.ok(Array.isArray(models.data) && models.data.length > 0, 'model/list returned no models');
  assert.ok(
    models.data.some((entry) => entry.model === model || entry.id === model),
    `model/list did not include ${model}`,
  );

  const started = await client.startThread({ cwd: workspace, model }, seed);
  const threadId = started.response.thread.id;
  const turn = await client.startTurn({
    threadId,
    input: [{ type: 'text', text: 'Reply exactly CODEX-V2-READY. Do not call tools.' }],
    mode: 'normal',
    model,
  }, { ...seed, attemptId: 'codex-v2-attempt-2' });
  await waitForProjected(
    (event) => event.type === 'turnCompleted' && event.turn.id === turn.response.turn.id,
    'materializing turn completion',
  );

  const listed = await client.requestCodex('thread/list', {
    limit: 100,
    cwd: workspace,
  });
  assert.ok(listed.data.some((thread) => thread.id === threadId), 'thread/list lost the live thread');

  const read = await client.requestCodex('thread/read', { threadId, includeTurns: true });
  assert.equal(read.thread.id, threadId, 'thread/read returned the wrong thread');
  assert.ok(read.thread.turns.length > 0, 'thread/read did not include materialized turns');

  const rawRead = await client.requestRaw('thread/read', { threadId, includeTurns: false });
  assert.equal(rawRead.thread.id, threadId, 'requestRaw did not return the exact result');

  await client.requestCodex('thread/archive', { threadId });
  const archivedNotification = await waitForCodexNotification('thread/archived');
  assert.equal(archivedNotification.params.threadId, threadId);
  const archived = await client.requestCodex('thread/list', {
    limit: 100,
    archived: true,
    cwd: workspace,
  });
  assert.ok(archived.data.some((thread) => thread.id === threadId), 'archived list lost the thread');

  const unarchived = await client.requestCodex('thread/unarchive', { threadId });
  assert.equal(unarchived.thread.id, threadId, 'thread/unarchive returned the wrong thread');
  const unarchivedNotification = await waitForCodexNotification('thread/unarchived');
  assert.equal(unarchivedNotification.params.threadId, threadId);

  const restored = await client.requestCodex('thread/list', {
    limit: 100,
    cwd: workspace,
  });
  assert.ok(restored.data.some((thread) => thread.id === threadId), 'unarchived thread was not restored');

  await client.close();
  client = undefined;
  console.log(JSON.stringify({
    phase: 'codexV2Accepted',
    threadId,
    sourceCodexRevision: CODEX_V2_PROTOCOL_MANIFEST.sourceCodexRevision,
    schemaSha256: CODEX_V2_PROTOCOL_MANIFEST.schemaSha256,
    clientMethodCount: CODEX_V2_PROTOCOL_MANIFEST.clientRequestMethods.length,
    modelCount: models.data.length,
    lifecycle: [
      'config/read',
      'model/list',
      'thread/list',
      'thread/read',
      'thread/archive',
      'thread/unarchive',
    ],
    exactNotifications: ['thread/archived', 'thread/unarchived'],
  }));
} finally {
  if (client) await client.close().catch(() => undefined);
  await rm(temporaryRoot, { recursive: true, force: true });
}
