import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import {
  access,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  readlink,
  rm,
  writeFile,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { FactoryClient } from '../../../../harness-client/dist/index.js';

const scriptRoot = dirname(fileURLToPath(import.meta.url));
const runtimePath = process.env.FACTORY_RUNTIME_PATH ?? resolve(
  scriptRoot,
  '..',
  '..',
  'target',
  'debug',
  'factory-runtime',
);
const providerBaseUrl = process.env.FACTORY_PROVIDER_BASE_URL ?? 'http://127.0.0.1:18102/v1';
const modelCatalogJson = process.env.FACTORY_MODEL_CATALOG_JSON ??
  '/tmp/software-factory-provider-glm52/codex-models.json';
const model = process.env.FACTORY_ZAI_MODEL ?? 'glm-5.2';
const timeoutMs = Number(process.env.FACTORY_SMOKE_TIMEOUT_MS ?? '300000');
const trace = process.env.FACTORY_SMOKE_TRACE === '1';
const qdrantImage = process.env.FACTORY_QDRANT_IMAGE ?? 'qdrant/qdrant:v1.16';

await access(runtimePath);
if (!isAbsolute(modelCatalogJson)) {
  throw new Error('FACTORY_MODEL_CATALOG_JSON must be an absolute path');
}
await access(modelCatalogJson);
if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
  throw new Error('FACTORY_SMOKE_TIMEOUT_MS must be a positive number');
}

const runToken = randomUUID().replaceAll('-', '');
const codename = `MARBLE-COMET-${runToken.slice(0, 10).toUpperCase()}`;
const fact = `The maintenance codename for Project Alder is ${codename}.`;
const rememberArgs = {
  content: fact,
  tags: ['acceptance', 'project-alder'],
};
const recallArgs = {
  query: 'maintenance codename Project Alder',
  limit: 3,
};
const question = 'What is the maintenance codename for Project Alder?';
const collection = process.env.FACTORY_QDRANT_COLLECTION ?? `factory_memory_smoke_${runToken}`;
const namespace = process.env.FACTORY_MEMORY_NAMESPACE ?? `acceptance-${runToken}`;
const externalQdrantUrl = process.env.FACTORY_QDRANT_URL;
const qdrantApiKey = process.env.FACTORY_QDRANT_API_KEY;
const containerName = `factory-qdrant-smoke-${runToken.slice(0, 12)}`;
let ownsQdrant = false;
let qdrantUrl = externalQdrantUrl;
let temporaryRoot;
let client;
let succeeded = false;

try {
  if (!qdrantUrl) {
    docker([
      'run',
      '--detach',
      '--rm',
      '--name',
      containerName,
      '--publish',
      '127.0.0.1::6333',
      qdrantImage,
    ]);
    ownsQdrant = true;
    const portOutput = docker(['port', containerName, '6333/tcp']).trim();
    const port = portOutput.match(/:(\d+)$/)?.[1];
    if (!port) throw new Error(`could not resolve disposable Qdrant port from ${portOutput}`);
    qdrantUrl = `http://127.0.0.1:${port}`;
  }
  await waitForHttp(new URL('/healthz', qdrantUrl), 'disposable Qdrant');
  await waitForHttp(new URL('/healthz', providerBaseUrl), 'GLM provider bridge');

  temporaryRoot = await mkdtemp(resolve(tmpdir(), 'factory-glm-qdrant-memory-smoke-'));
  const codexHome = resolve(temporaryRoot, 'codex-home');
  const workspace = resolve(temporaryRoot, 'workspace');
  await mkdir(codexHome, { recursive: true });
  await mkdir(workspace, { recursive: true });
  await writeFile(resolve(workspace, 'README.md'), '# Factory memory acceptance fixture\n');
  await writeFile(resolve(codexHome, 'config.toml'), [
    `model = ${JSON.stringify(model)}`,
    'model_provider = "factory-provider"',
    'model_context_window = 1000000',
    'model_reasoning_effort = "medium"',
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

  const baseline = await snapshotTree(workspace);
  const events = [];
  const errors = [];
  const seed = {
    jobId: 'glm-qdrant-memory-proof',
    operationId: 'native-cross-thread-memory',
    workflowRunId: `glm-qdrant-memory-run-${runToken}`,
    taskRunExternalId: `glm-qdrant-memory-task-${runToken}`,
  };

  function attempt(number) {
    return { ...seed, attemptId: `glm-qdrant-memory-attempt-${number}` };
  }

  async function connectRuntime() {
    const env = {
      CODEX_HOME: codexHome,
      FACTORY_QDRANT_URL: qdrantUrl,
      FACTORY_QDRANT_COLLECTION: collection,
      FACTORY_MEMORY_NAMESPACE: namespace,
    };
    if (qdrantApiKey) env.FACTORY_QDRANT_API_KEY = qdrantApiKey;
    return FactoryClient.connect({
      runtimePath,
      cwd: workspace,
      env,
      onEvent: (correlated) => {
        events.push(correlated);
        if (trace && correlated.event.type !== 'itemDelta') {
          const event = correlated.event;
          console.error(JSON.stringify({
            type: event.type,
            itemType: 'item' in event ? event.item.type : undefined,
            threadId: 'threadId' in event ? event.threadId : undefined,
            turnId: 'turnId' in event ? event.turnId : event.turn?.id,
          }));
        }
      },
      onError: (error) => errors.push(error),
    });
  }

  async function waitForEvent(predicate, description) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const match = events.find(({ event }) => predicate(event));
      if (match) return match.event;
      const failure = events.find(({ event }) => event.type === 'runtimeError');
      if (failure) {
        throw new Error(`runtime failed while waiting for ${description}: ${JSON.stringify(failure.event.error)}`);
      }
      if (errors.length > 0) {
        throw new Error(`runtime connection failed while waiting for ${description}: ${errors.at(-1).message}`);
      }
      await delay(50);
    }
    throw new Error(`timed out waiting for ${description}`);
  }

  async function rolloutItems(turnId, itemType) {
    const records = await readRolloutRecords(resolve(codexHome, 'sessions'));
    return records
      .filter(({ record }) =>
        record?.type === 'response_item' &&
        record.payload?.type === itemType &&
        record.payload?.internal_chat_message_metadata_passthrough?.turn_id === turnId,
      )
      .map(({ record }) => record.payload);
  }

  function completedMessages(turnId) {
    return events
      .map(({ event }) => event)
      .filter((event) =>
        event.type === 'itemCompleted' &&
        event.turnId === turnId &&
        event.item.type === 'agentMessage',
      )
      .map((event) => event.item.text);
  }

  async function assertWorkspaceUnchanged(stage) {
    assert.deepStrictEqual(
      await snapshotTree(workspace),
      baseline,
      `workspace changed during ${stage}`,
    );
  }

  client = await connectRuntime();
  const firstStarted = await client.startThread({
    model,
    modelProvider: 'factory-provider',
    cwd: workspace,
    approvalPolicy: 'never',
    sandbox: 'danger-full-access',
    developerInstructions: [
      'This is a strict Factory long-term-memory acceptance.',
      'Do not inspect or modify workspace files.',
      'Call only the explicitly requested Factory memory tool with the exact supplied JSON.',
    ].join(' '),
  }, attempt(1));
  const sourceThreadId = firstStarted.response.thread.id;
  const rememberTurn = await client.startTurn({
    threadId: sourceThreadId,
    mode: 'normal',
    model,
    input: [{
      type: 'text',
      text: [
        `Call factory_remember exactly once with ${JSON.stringify(rememberArgs)}.`,
        'Do not call any other tool.',
        'After its successful receipt, reply with exactly MEMORY_STORED.',
      ].join('\n'),
    }],
  }, attempt(2));
  const rememberTurnId = rememberTurn.response.turn.id;
  const rememberTerminal = await waitForEvent(
    (event) => event.type === 'turnCompleted' && event.turn.id === rememberTurnId,
    'the GLM remember turn to complete',
  );
  assert.equal(rememberTerminal.turn.status, 'completed');
  const rememberCalls = await rolloutItems(rememberTurnId, 'function_call');
  assert.deepStrictEqual(rememberCalls.map((call) => call.name), ['factory_remember']);
  assert.deepStrictEqual(JSON.parse(rememberCalls[0].arguments), rememberArgs);
  const rememberOutputs = await rolloutItems(rememberTurnId, 'function_call_output');
  assert.equal(rememberOutputs.length, 1);
  assert.equal(rememberOutputs[0].call_id, rememberCalls[0].call_id);
  const rememberReceipt = JSON.parse(rememberOutputs[0].output);
  assert.deepStrictEqual(
    {
      namespace: rememberReceipt.namespace,
      stored: rememberReceipt.stored,
      tag_count: rememberReceipt.tag_count,
    },
    { namespace, stored: true, tag_count: 2 },
  );
  assert.match(rememberReceipt.id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.ok(!Number.isNaN(Date.parse(rememberReceipt.created_at)));
  assert.ok(completedMessages(rememberTurnId).some((text) => text.trim() === 'MEMORY_STORED'));

  const storedPoint = await loadQdrantPoint(qdrantUrl, collection, rememberReceipt.id, qdrantApiKey);
  const expectedPayload = {
    id: rememberReceipt.id,
    content: fact,
    namespace,
    tags: rememberArgs.tags,
    source_thread_id: sourceThreadId,
    created_at: rememberReceipt.created_at,
    updated_at: rememberReceipt.created_at,
    vectorizer: 'factory-lexical-fnv1a-v1',
  };
  assert.deepStrictEqual(storedPoint.payload, expectedPayload);
  const sparseVector = storedPoint.vector?.factory_lexical;
  assert.ok(Array.isArray(sparseVector?.indices) && sparseVector.indices.length > 0);
  assert.equal(sparseVector.indices.length, sparseVector.values.length);
  await assertWorkspaceUnchanged('the remember thread');

  await client.close();
  client = undefined;

  client = await connectRuntime();
  const secondStarted = await client.startThread({
    model,
    modelProvider: 'factory-provider',
    cwd: workspace,
    approvalPolicy: 'never',
    sandbox: 'danger-full-access',
    developerInstructions: [
      'Use relevant <factory_memory_context> supplied in developer input.',
      'Do not inspect or modify workspace files.',
      'When the user forbids tools, answer from injected memory without calling one.',
    ].join(' '),
  }, attempt(3));
  const recallThreadId = secondStarted.response.thread.id;
  assert.notEqual(recallThreadId, sourceThreadId, 'memory proof reused the source Codex thread');

  const automaticTurn = await client.startTurn({
    threadId: recallThreadId,
    mode: 'normal',
    model,
    input: [{
      type: 'text',
      text: `${question}\nDo not call any tools. Reply with the codename only.`,
    }],
  }, attempt(4));
  const automaticTurnId = automaticTurn.response.turn.id;
  const automaticTerminal = await waitForEvent(
    (event) => event.type === 'turnCompleted' && event.turn.id === automaticTurnId,
    'the cross-thread automatic-memory turn to complete',
  );
  assert.equal(automaticTerminal.turn.status, 'completed');
  assert.deepStrictEqual(await rolloutItems(automaticTurnId, 'function_call'), []);
  const automaticMessages = completedMessages(automaticTurnId);
  assert.ok(automaticMessages.length > 0);
  assert.equal(automaticMessages.at(-1).trim(), codename);

  const injectedContext = await loadInjectedContext(codexHome, recallThreadId, fact);
  assert.equal(injectedContext.source, 'factory-qdrant-memory');
  assert.equal(injectedContext.namespace, namespace);
  assert.equal(
    injectedContext.query,
    `${question}\nDo not call any tools. Reply with the codename only.`,
  );
  assert.ok(injectedContext.memories.length >= 1);
  assert.deepStrictEqual(memoryWithoutScore(injectedContext.memories[0]), expectedPayload);
  assert.ok(injectedContext.memories[0].score > 0);

  const explicitTurn = await client.startTurn({
    threadId: recallThreadId,
    mode: 'normal',
    model,
    input: [{
      type: 'text',
      text: [
        `Call factory_recall exactly once with ${JSON.stringify(recallArgs)}.`,
        'Do not call any other tool.',
        'After its successful result, reply with exactly RECALL_COMPLETE.',
      ].join('\n'),
    }],
  }, attempt(5));
  const explicitTurnId = explicitTurn.response.turn.id;
  const explicitTerminal = await waitForEvent(
    (event) => event.type === 'turnCompleted' && event.turn.id === explicitTurnId,
    'the explicit factory_recall turn to complete',
  );
  assert.equal(explicitTerminal.turn.status, 'completed');
  const recallCalls = await rolloutItems(explicitTurnId, 'function_call');
  assert.deepStrictEqual(recallCalls.map((call) => call.name), ['factory_recall']);
  assert.deepStrictEqual(JSON.parse(recallCalls[0].arguments), recallArgs);
  const recallOutputs = await rolloutItems(explicitTurnId, 'function_call_output');
  assert.equal(recallOutputs.length, 1);
  assert.equal(recallOutputs[0].call_id, recallCalls[0].call_id);
  const recallReceipt = JSON.parse(recallOutputs[0].output);
  assert.equal(recallReceipt.namespace, namespace);
  assert.equal(recallReceipt.count, 1);
  assert.equal(recallReceipt.memories.length, 1);
  assert.deepStrictEqual(memoryWithoutScore(recallReceipt.memories[0]), expectedPayload);
  assert.ok(recallReceipt.memories[0].score > 0);
  assert.ok(completedMessages(explicitTurnId).some((text) => text.trim() === 'RECALL_COMPLETE'));

  const persistedPoint = await loadQdrantPoint(
    qdrantUrl,
    collection,
    rememberReceipt.id,
    qdrantApiKey,
  );
  assert.deepStrictEqual(persistedPoint.payload, expectedPayload);
  await assertWorkspaceUnchanged('runtime restart, cross-thread injection, and explicit recall');

  succeeded = true;
  console.log(JSON.stringify({
    ok: true,
    model,
    providerBaseUrl,
    qdrantUrl,
    qdrantImage: ownsQdrant ? qdrantImage : null,
    collection,
    namespace,
    sourceThreadId,
    recallThreadId,
    rememberTurnId,
    automaticTurnId,
    explicitTurnId,
    rememberReceipt,
    automaticAnswer: automaticMessages.at(-1).trim(),
    automaticContextMemoryId: injectedContext.memories[0].id,
    recallCount: recallReceipt.count,
    recallMemoryId: recallReceipt.memories[0].id,
    qdrantPayloadPersisted: true,
    runtimeRestarted: true,
    distinctThread: true,
    workspaceUnchanged: true,
  }));
} finally {
  if (client) await client.close().catch(() => undefined);
  if (temporaryRoot && (succeeded || process.env.FACTORY_KEEP_SMOKE_DIR !== '1')) {
    await rm(temporaryRoot, { recursive: true, force: true });
  } else if (temporaryRoot && !succeeded) {
    console.error(`preserved failed smoke workspace at ${temporaryRoot}`);
  }
  if (ownsQdrant) {
    docker(['stop', '--time', '1', containerName], true);
  }
}

function docker(args, allowFailure = false) {
  const result = spawnSync('docker', args, { encoding: 'utf8' });
  if (!allowFailure && result.status !== 0) {
    throw new Error(`docker ${args[0]} failed: ${(result.stderr || result.stdout).trim()}`);
  }
  return result.stdout ?? '';
}

async function waitForHttp(url, description) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // The disposable service is still starting.
    }
    await delay(250);
  }
  throw new Error(`timed out waiting for ${description} at ${url}`);
}

function delay(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}

async function loadQdrantPoint(baseUrl, collection, id, apiKey) {
  const url = new URL(
    `/collections/${encodeURIComponent(collection)}/points/${encodeURIComponent(id)}`,
    baseUrl,
  );
  url.searchParams.set('with_payload', 'true');
  url.searchParams.set('with_vector', 'true');
  const headers = apiKey ? { 'api-key': apiKey } : undefined;
  const response = await fetch(url, { headers });
  if (!response.ok) {
    throw new Error(`Qdrant point read failed with HTTP ${response.status}: ${await response.text()}`);
  }
  return (await response.json()).result;
}

function memoryWithoutScore(memory) {
  const { score: _score, ...payload } = memory;
  return payload;
}

async function loadInjectedContext(codexHome, threadId, expectedContent) {
  const records = await readRolloutRecords(resolve(codexHome, 'sessions'));
  for (const { path, record } of records) {
    if (!path.includes(threadId) || record?.type !== 'response_item') continue;
    const payload = record.payload;
    if (payload?.type !== 'message' || payload.role !== 'developer') continue;
    for (const content of payload.content ?? []) {
      if (content.type !== 'input_text' || typeof content.text !== 'string') continue;
      const match = content.text.match(/<factory_memory_context>([\s\S]+)<\/factory_memory_context>/);
      if (!match) continue;
      const context = JSON.parse(match[1]);
      if (context.memories?.some((memory) => memory.content === expectedContent)) return context;
    }
  }
  throw new Error('fresh thread rollout did not contain the expected Factory memory context');
}

async function snapshotTree(root) {
  const snapshot = [];
  async function visit(directory) {
    const entries = await readdir(directory, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const absolute = resolve(directory, entry.name);
      const path = relative(root, absolute);
      const metadata = await lstat(absolute);
      if (entry.isDirectory()) {
        snapshot.push({ path, type: 'directory', mode: metadata.mode });
        await visit(absolute);
      } else if (entry.isSymbolicLink()) {
        snapshot.push({ path, type: 'symlink', target: await readlink(absolute) });
      } else {
        const content = await readFile(absolute);
        snapshot.push({
          path,
          type: 'file',
          mode: metadata.mode,
          bytes: content.length,
          sha256: createHash('sha256').update(content).digest('hex'),
        });
      }
    }
  }
  await visit(root);
  return snapshot;
}

async function readRolloutRecords(root) {
  const records = [];
  async function visit(directory) {
    const entries = await readdir(directory, { withFileTypes: true });
    for (const entry of entries) {
      const absolute = resolve(directory, entry.name);
      if (entry.isDirectory()) {
        await visit(absolute);
      } else if (entry.isFile() && entry.name.endsWith('.jsonl')) {
        const lines = (await readFile(absolute, 'utf8')).split('\n').filter(Boolean);
        records.push(...lines.map((line) => ({ path: absolute, record: JSON.parse(line) })));
      }
    }
  }
  await visit(root);
  return records;
}
