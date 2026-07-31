import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { spawn as spawnProcess, spawnSync } from 'node:child_process';
import { access, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { FactoryClient } from '../../../../harness-client/dist/index.js';

const scriptRoot = dirname(fileURLToPath(import.meta.url));
const factoryRoot = resolve(scriptRoot, '..', '..');
const runtimePath = process.env.FACTORY_RUNTIME_PATH ??
  resolve(factoryRoot, 'target', 'debug', 'factory-runtime');
const factorydPath = process.env.FACTORYD_PATH ??
  resolve(factoryRoot, 'target', 'debug', 'factoryd');
const providerBaseUrl = process.env.FACTORY_PROVIDER_BASE_URL ??
  'http://127.0.0.1:18102/v1';
const modelCatalogJson = process.env.FACTORY_MODEL_CATALOG_JSON ??
  '/tmp/software-factory-provider-glm52/codex-models.json';
const model = process.env.FACTORY_ZAI_MODEL ?? 'glm-5.2';
const postgresImage = process.env.FACTORY_POSTGRES_IMAGE ?? 'postgres:16-alpine';
const timeoutMs = Number(process.env.FACTORY_SMOKE_TIMEOUT_MS ?? '300000');
const trace = process.env.FACTORY_SMOKE_TRACE === '1';

await access(runtimePath);
await access(factorydPath);
if (!isAbsolute(modelCatalogJson)) {
  throw new Error('FACTORY_MODEL_CATALOG_JSON must be an absolute path');
}
await access(modelCatalogJson);
if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
  throw new Error('FACTORY_SMOKE_TIMEOUT_MS must be a positive number');
}

const runToken = randomUUID().replaceAll('-', '');
const childResult = `CHILD_RESULT_${runToken.slice(0, 12).toUpperCase()}`;
const parentResult = `PARENT_SEQUENCE_COMPLETE_${runToken.slice(0, 12).toUpperCase()}`;
const taskName = `proof_child_${runToken.slice(0, 10)}`;
const containerName = `factory-subagent-postgres-${runToken.slice(0, 12)}`;
let temporaryRoot;
let client;
let factoryd;
let ownsPostgres = false;
let succeeded = false;

try {
  docker([
    'run',
    '--detach',
    '--rm',
    '--name',
    containerName,
    '--env',
    'POSTGRES_PASSWORD=factory',
    '--env',
    'POSTGRES_DB=factory',
    '--publish',
    '127.0.0.1::5432',
    postgresImage,
  ]);
  ownsPostgres = true;
  const portOutput = docker(['port', containerName, '5432/tcp']).trim();
  const postgresPort = portOutput.match(/:(\d+)$/)?.[1];
  if (!postgresPort) {
    throw new Error(`could not resolve disposable PostgreSQL port from ${portOutput}`);
  }
  await waitForPostgres(containerName);
  const databaseUrl = `postgres://postgres:factory@127.0.0.1:${postgresPort}/factory`;
  run(factorydPath, ['--database-url', databaseUrl, 'migrate']);
  factoryd = await startFactoryd(databaseUrl);
  await waitForHttp(new URL('/healthz', factoryd.baseUrl), 'factoryd');
  await waitForHttp(new URL('/healthz', providerBaseUrl), 'GLM provider bridge');

  temporaryRoot = await mkdtemp(resolve(tmpdir(), 'factory-glm-subagent-smoke-'));
  const codexHome = resolve(temporaryRoot, 'codex-home');
  const workspace = resolve(temporaryRoot, 'workspace');
  await mkdir(codexHome, { recursive: true });
  await mkdir(workspace, { recursive: true });
  await writeFile(resolve(workspace, 'README.md'), '# Native subagent acceptance fixture\n');
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

  const events = [];
  const errors = [];
  const seed = {
    jobId: 'glm-native-subagent-proof',
    operationId: 'native-subagent-durability',
    workflowRunId: `glm-native-subagent-run-${runToken}`,
    taskRunExternalId: `glm-native-subagent-task-${runToken}`,
  };
  const attempt = (number) => ({
    ...seed,
    attemptId: `glm-native-subagent-attempt-${number}`,
  });

  async function connectRuntime() {
    return FactoryClient.connect({
      runtimePath,
      cwd: workspace,
      env: {
        CODEX_HOME: codexHome,
        FACTORYD_URL: `${factoryd.baseUrl}v1`,
      },
      onEvent: (correlated) => {
        events.push(correlated);
        if (trace && correlated.event.type !== 'itemDelta') {
          const event = correlated.event;
          console.error(JSON.stringify({
            type: event.type,
            itemType: 'item' in event ? event.item.type : undefined,
            tool: 'item' in event ? event.item.tool : undefined,
            status: 'item' in event ? event.item.status : undefined,
            threadId: 'threadId' in event ? event.threadId : undefined,
            turnId: 'turnId' in event ? event.turnId : event.turn?.id,
          }));
        }
      },
      onError: (error) => errors.push(error),
    });
  }

  async function waitForTurn(turnId, description) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const terminal = events
        .map(({ event }) => event)
        .find((event) => event.type === 'turnCompleted' && event.turn.id === turnId);
      if (terminal) return terminal;
      const failure = events
        .map(({ event }) => event)
        .find((event) => event.type === 'runtimeError');
      if (failure) {
        throw new Error(`runtime failed while waiting for ${description}: ${JSON.stringify(failure.error)}`);
      }
      if (errors.length > 0) {
        throw new Error(`runtime connection failed while waiting for ${description}: ${errors.at(-1).message}`);
      }
      await delay(50);
    }
    throw new Error(`timed out waiting for ${description}`);
  }

  client = await connectRuntime();
  const started = await client.startThread({
    model,
    modelProvider: 'factory-provider',
    cwd: workspace,
    approvalPolicy: 'never',
    sandbox: 'danger-full-access',
    developerInstructions: [
      'This is a strict native Codex subagent acceptance.',
      'Use only native collaboration primitives; do not inspect or modify files.',
      'You must spawn exactly one child, call the native wait primitive at least once, receive its result, then terminate it with close_agent or interrupt_agent.',
      `The child task name is ${taskName}.`,
      `The child must receive this instruction: Reply with exactly ${childResult}. Do not call tools.`,
      `Your final reply must contain both ${parentResult} and ${childResult}.`,
    ].join(' '),
  }, attempt(1));
  const parentThreadId = started.response.thread.id;
  const turn = await client.startTurn({
    threadId: parentThreadId,
    mode: 'normal',
    model,
    input: [{
      type: 'text',
      text: [
        'Execute the required subagent sequence now.',
        'Even if the child result arrives automatically, still call the native wait primitive.',
        'After receiving the exact child result, close or interrupt that child before your final reply.',
        'Do not call any non-collaboration tool.',
      ].join('\n'),
    }],
  }, attempt(2));
  const turnId = turn.response.turn.id;
  const terminal = await waitForTurn(turnId, 'the native subagent turn');
  assert.equal(terminal.turn.status, 'completed');

  const functionCalls = await rolloutItems(codexHome, turnId, 'function_call');
  const toolNames = functionCalls.map((call) => call.name);
  assert.equal(toolNames.filter((name) => name === 'spawn_agent').length, 1);
  assert.ok(toolNames.includes('wait_agent'), `missing wait_agent call: ${toolNames.join(', ')}`);
  const terminalTool = toolNames.find((name) =>
    name === 'close_agent' || name === 'interrupt_agent');
  assert.ok(terminalTool, `missing close_agent/interrupt_agent call: ${toolNames.join(', ')}`);
  assert.ok(
    toolNames.every((name) => [
      'spawn_agent',
      'wait_agent',
      'close_agent',
      'interrupt_agent',
    ].includes(name)),
    `unexpected non-collaboration tool call: ${toolNames.join(', ')}`,
  );

  const parentMessages = events
    .map(({ event }) => event)
    .filter((event) =>
      event.type === 'itemCompleted' &&
      event.turnId === turnId &&
      event.item.type === 'agentMessage')
    .map((event) => event.item.text);
  assert.ok(parentMessages.some((message) => message.includes(parentResult)));
  assert.ok(parentMessages.some((message) => message.includes(childResult)));

  const beforeRestart = await loadFactoryState(factoryd.baseUrl, parentThreadId);
  const activities = beforeRestart.state.subagents?.activities;
  assert.ok(Array.isArray(activities), 'factoryd state is missing subagent activities');
  const spawnActivity = activities.find((activity) => activity.tool === 'spawn_agent');
  const waitActivity = activities.find((activity) => activity.tool === 'wait');
  const terminalActivity = activities.find((activity) =>
    activity.tool === 'close_agent' || activity.tool === 'interrupt_agent');
  assert.ok(spawnActivity, 'Factory state is missing native spawn activity');
  assert.ok(waitActivity, 'Factory state is missing native wait activity');
  assert.ok(terminalActivity, 'Factory state is missing native terminal activity');
  assert.equal(spawnActivity.sender_thread_id, parentThreadId);
  assert.equal(spawnActivity.status, 'completed');
  assert.equal(waitActivity.sender_thread_id, parentThreadId);
  assert.equal(waitActivity.status, 'completed');
  assert.equal(terminalActivity.sender_thread_id, parentThreadId);
  assert.equal(terminalActivity.status, 'completed');
  const childThreadId = spawnActivity.receiver_thread_ids[0];
  assert.match(childThreadId, /^[0-9a-f-]{36}$/);
  assert.ok(
    terminalActivity.receiver_thread_ids.includes(childThreadId),
    'terminal activity does not target the spawned child',
  );
  if (spawnActivity.prompt !== null) {
    assert.ok(spawnActivity.prompt.includes(childResult));
  }

  await client.close();
  client = undefined;

  client = await connectRuntime();
  const resumed = await client.resumeThread({ threadId: parentThreadId }, attempt(3));
  assert.equal(resumed.response.thread.id, parentThreadId);
  const afterRestart = await loadFactoryState(factoryd.baseUrl, parentThreadId);
  assert.deepStrictEqual(afterRestart.state.subagents, beforeRestart.state.subagents);

  const childRead = await client.requestRaw('thread/read', {
    threadId: childThreadId,
    includeTurns: true,
  });
  assert.equal(childRead.thread.id, childThreadId);
  assert.equal(childRead.thread.parentThreadId, parentThreadId);
  assert.ok(JSON.stringify(childRead.thread.turns).includes(childResult));
  const childList = await client.requestRaw('thread/list', {
    parentThreadId,
    limit: 25,
  });
  assert.ok(childList.data.some((thread) => thread.id === childThreadId));

  succeeded = true;
  console.log(JSON.stringify({
    ok: true,
    model,
    providerBaseUrl,
    factorydUrl: `${factoryd.baseUrl}v1`,
    postgresImage,
    parentThreadId,
    childThreadId,
    turnId,
    toolNames,
    terminalTool,
    factorydRevisionBeforeRestart: beforeRestart.revision,
    factorydRevisionAfterRestart: afterRestart.revision,
    activities,
    childReadParentThreadId: childRead.thread.parentThreadId,
    childListed: true,
    childResultObserved: true,
    runtimeRestarted: true,
  }));
} finally {
  if (client) await client.close().catch(() => undefined);
  if (factoryd) await stopProcess(factoryd.child);
  if (temporaryRoot && (succeeded || process.env.FACTORY_KEEP_SMOKE_DIR !== '1')) {
    await rm(temporaryRoot, { recursive: true, force: true });
  } else if (temporaryRoot && !succeeded) {
    console.error(`preserved failed smoke workspace at ${temporaryRoot}`);
  }
  if (ownsPostgres) docker(['stop', '--time', '1', containerName], true);
}

function run(command, args) {
  const result = spawnSync(command, args, { encoding: 'utf8' });
  if (result.status !== 0) {
    throw new Error(`${command} failed: ${(result.stderr || result.stdout).trim()}`);
  }
  return result.stdout ?? '';
}

function docker(args, allowFailure = false) {
  const result = spawnSync('docker', args, { encoding: 'utf8' });
  if (!allowFailure && result.status !== 0) {
    throw new Error(`docker ${args[0]} failed: ${(result.stderr || result.stdout).trim()}`);
  }
  return result.stdout ?? '';
}

async function waitForPostgres(name) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    const result = spawnSync(
      'docker',
      ['exec', name, 'pg_isready', '--username', 'postgres', '--dbname', 'factory'],
      { encoding: 'utf8' },
    );
    if (result.status === 0) return;
    await delay(250);
  }
  throw new Error('timed out waiting for disposable PostgreSQL');
}

async function startFactoryd(databaseUrl) {
  const child = spawnProcess(
    factorydPath,
    ['--database-url', databaseUrl, 'serve', '--bind', '127.0.0.1:0'],
    { stdio: ['ignore', 'pipe', 'pipe'] },
  );
  let stdout = '';
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr += chunk.toString(); });
  return new Promise((resolvePromise, rejectPromise) => {
    const deadline = setTimeout(() => {
      rejectPromise(new Error(`timed out starting factoryd: ${stderr}`));
    }, 30_000);
    child.stdout.on('data', (chunk) => {
      stdout += chunk.toString();
      const newline = stdout.indexOf('\n');
      if (newline < 0) return;
      try {
        const receipt = JSON.parse(stdout.slice(0, newline));
        clearTimeout(deadline);
        resolvePromise({ child, baseUrl: `http://${receipt.listening}/` });
      } catch (error) {
        clearTimeout(deadline);
        rejectPromise(new Error(`invalid factoryd startup receipt: ${stdout}\n${stderr}`, {
          cause: error,
        }));
      }
    });
    child.once('exit', (code, signal) => {
      clearTimeout(deadline);
      rejectPromise(new Error(`factoryd exited during startup (${code ?? signal}): ${stderr}`));
    });
  });
}

async function stopProcess(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  child.kill('SIGTERM');
  await Promise.race([once(child, 'exit'), delay(5_000)]);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill('SIGKILL');
    await once(child, 'exit');
  }
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

async function loadFactoryState(baseUrl, threadId) {
  const response = await fetch(new URL(
    `v1/threads/${encodeURIComponent(threadId)}/state`,
    baseUrl,
  ));
  if (!response.ok) {
    throw new Error(`factoryd state read failed with HTTP ${response.status}: ${await response.text()}`);
  }
  return response.json();
}

async function rolloutItems(codexHome, turnId, itemType) {
  const { readdir, readFile } = await import('node:fs/promises');
  const records = [];
  async function visit(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) {
        await visit(path);
      } else if (entry.isFile() && entry.name.endsWith('.jsonl')) {
        const lines = (await readFile(path, 'utf8')).split('\n').filter(Boolean);
        for (const line of lines) records.push(JSON.parse(line));
      }
    }
  }
  await visit(resolve(codexHome, 'sessions'));
  return records
    .filter((record) =>
      record?.type === 'response_item' &&
      record.payload?.type === itemType &&
      record.payload?.internal_chat_message_metadata_passthrough?.turn_id === turnId)
    .map((record) => record.payload);
}

function delay(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}
