import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
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

import { FactoryClient } from '../dist/index.js';

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
const factorydUrl = process.env.FACTORYD_URL ?? 'http://127.0.0.1:8787/v1';
const providerBaseUrl = process.env.FACTORY_PROVIDER_BASE_URL ?? 'http://127.0.0.1:18102/v1';
const modelCatalogJson = process.env.FACTORY_MODEL_CATALOG_JSON ??
  '/tmp/software-factory-provider-glm52/codex-models.json';
const model = process.env.FACTORY_ZAI_MODEL ?? 'glm-5.2';
const timeoutMs = Number(process.env.FACTORY_SMOKE_TIMEOUT_MS ?? '300000');
const trace = process.env.FACTORY_SMOKE_TRACE === '1';

await access(runtimePath);
if (!isAbsolute(modelCatalogJson)) {
  throw new Error('FACTORY_MODEL_CATALOG_JSON must be an absolute path');
}
await access(modelCatalogJson);
if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
  throw new Error('FACTORY_SMOKE_TIMEOUT_MS must be a positive number');
}
const factorydBaseUrl = new URL(factorydUrl.endsWith('/') ? factorydUrl : `${factorydUrl}/`);

const decomposition = {
  units: [
    {
      id: 'design',
      title: 'Design checkpoint contract',
      description: 'Define the durable checkpoint record and resume boundary.',
      depends_on: [],
    },
    {
      id: 'implement',
      title: 'Implement checkpoint persistence',
      description: 'Persist completed-stage checkpoints through the job runner.',
      depends_on: ['design'],
    },
    {
      id: 'verify',
      title: 'Verify interrupted-job resume',
      description: 'Prove recovery resumes after the last completed stage.',
      depends_on: ['implement'],
    },
  ],
};
const progress = {
  unit_id: 'design',
  status: 'in_progress',
  summary: 'Checkpoint contract drafted for review.',
};
const review = {
  verdict: 'request_changes',
  summary: 'Checkpoint identity needs an explicit compatibility rule.',
  findings: [{
    id: 'REV-1',
    severity: 'major',
    unit_id: 'design',
    title: 'Checkpoint identity is underspecified',
    evidence: 'The design does not define how renamed stages map to stored checkpoints.',
    recommendation: 'Persist a stable stage ID and reject incompatible checkpoint graphs.',
  }],
};
const remediation = {
  dispositions: [{
    finding_id: 'REV-1',
    disposition: 'accepted',
    rationale: 'The design will use stable stage IDs and reject incompatible graphs.',
    unit_id: 'design',
  }],
};
const callsExpected = [
  ['factory_decompose', decomposition],
  ['factory_update_progress', progress],
  ['factory_record_review', review],
  ['factory_record_remediation', remediation],
];
const receiptsExpected = [
  { operation: 'decompose', revision: 1, work_unit_count: 3, finding_count: 0, remediation_count: 0 },
  { operation: 'progress', revision: 2, work_unit_count: 3, finding_count: 0, remediation_count: 0 },
  { operation: 'review', revision: 3, work_unit_count: 3, finding_count: 1, remediation_count: 0 },
  { operation: 'remediation', revision: 4, work_unit_count: 3, finding_count: 1, remediation_count: 1 },
];
const stateExpected = {
  revision: 4,
  work_units: decomposition.units.map((unit) => ({
    ...unit,
    status: unit.id === 'design' ? 'in_progress' : 'pending',
    progress_summary: unit.id === 'design' ? progress.summary : null,
  })),
  review,
  remediations: remediation.dispositions,
};
const factorydDocumentExpected = {
  decomposition: {
    revision: 4,
    work_units: decomposition.units,
  },
  progress: {
    work_units: stateExpected.work_units.map(({ id, status, progress_summary }) => ({
      id,
      status,
      progress_summary,
    })),
  },
  review,
  remediation: { records: remediation.dispositions },
};

const temporaryRoot = await mkdtemp(resolve(tmpdir(), 'factory-glm-factoryd-state-smoke-'));
const codexHome = resolve(temporaryRoot, 'codex-home');
const workspace = resolve(temporaryRoot, 'workspace');
await mkdir(codexHome, { recursive: true });
await mkdir(workspace, { recursive: true });
await writeFile(resolve(workspace, 'README.md'), '# Durable Factory state acceptance fixture\n');
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
const runId = randomUUID();
const seed = {
  jobId: 'glm-factoryd-state-proof',
  operationId: 'durable-native-extension-state',
  workflowRunId: `glm-factoryd-state-run-${runId}`,
  taskRunExternalId: `glm-factoryd-state-task-${runId}`,
};
let client;
let succeeded = false;

function attempt(number) {
  return { ...seed, attemptId: `glm-factoryd-state-attempt-${number}` };
}

async function connectRuntime() {
  return FactoryClient.connect({
    runtimePath,
    cwd: workspace,
    env: { CODEX_HOME: codexHome, FACTORYD_URL: factorydUrl },
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
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 50));
  }
  throw new Error(`timed out waiting for ${description}`);
}

async function rolloutItems(turnId, itemType) {
  const records = await readRolloutRecords(resolve(codexHome, 'sessions'));
  return records
    .filter((record) =>
      record?.type === 'response_item' &&
      record.payload?.type === itemType &&
      record.payload?.internal_chat_message_metadata_passthrough?.turn_id === turnId,
    )
    .map((record) => record.payload);
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

async function loadFactorydRecord(threadId) {
  const url = new URL(`threads/${encodeURIComponent(threadId)}/state`, factorydBaseUrl);
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`factoryd state read failed with HTTP ${response.status}: ${await response.text()}`);
  }
  return response.json();
}

function factoryContextFromMessages(messages) {
  for (const message of messages) {
    for (const content of message.content ?? []) {
      if (content.type !== 'input_text' || typeof content.text !== 'string') continue;
      const match = content.text.match(/<factory_state>([\s\S]+)<\/factory_state>/);
      if (match) return JSON.parse(match[1]);
    }
  }
  throw new Error('resumed turn rollout did not contain native <factory_state> context');
}

try {
  client = await connectRuntime();
  const started = await client.startThread({
    model,
    modelProvider: 'factory-provider',
    cwd: workspace,
    approvalPolicy: 'never',
    sandbox: 'danger-full-access',
    developerInstructions: [
      'This is a strict durable Factory native-tool acceptance.',
      'Do not inspect or modify workspace files.',
      'Call only the four explicitly requested Factory tools, exactly once each and in order.',
      'Use each supplied JSON object verbatim and wait for its receipt before continuing.',
      'On later turns, <factory_state> in developer context is authoritative.',
    ].join(' '),
  }, attempt(1));
  const threadId = started.response.thread.id;
  assert.equal(started.response.modelProvider, 'factory-provider');
  assert.equal(started.response.model, model);

  const mutationTurn = await client.startTurn({
    threadId,
    mode: 'normal',
    model,
    input: [{
      type: 'text',
      text: [
        'Make exactly these four calls, sequentially, with no other tool calls:',
        `1. factory_decompose ${JSON.stringify(decomposition)}`,
        `2. factory_update_progress ${JSON.stringify(progress)}`,
        `3. factory_record_review ${JSON.stringify(review)}`,
        `4. factory_record_remediation ${JSON.stringify(remediation)}`,
        'After all four successful receipts, reply with exactly FACTORYD_STATE_WRITTEN.',
      ].join('\n'),
    }],
  }, attempt(2));
  const mutationTurnId = mutationTurn.response.turn.id;
  const mutationTerminal = await waitForEvent(
    (event) => event.type === 'turnCompleted' && event.turn.id === mutationTurnId,
    'the durable Factory mutation turn to complete',
  );
  assert.equal(mutationTerminal.turn.status, 'completed');

  const functionCalls = await rolloutItems(mutationTurnId, 'function_call');
  assert.deepStrictEqual(
    functionCalls.map((call) => call.name),
    callsExpected.map(([name]) => name),
    'GLM did not call exactly the four Factory tools in order',
  );
  functionCalls.forEach((call, index) => {
    assert.deepStrictEqual(JSON.parse(call.arguments), callsExpected[index][1]);
  });
  const outputsByCallId = new Map(
    (await rolloutItems(mutationTurnId, 'function_call_output')).map((output) => [output.call_id, output]),
  );
  const receipts = functionCalls.map((call, index) => {
    const output = outputsByCallId.get(call.call_id);
    assert.ok(output, `missing output for ${call.name}`);
    const receipt = JSON.parse(output.output);
    assert.deepStrictEqual(receipt, receiptsExpected[index]);
    return receipt;
  });
  assert.ok(
    completedMessages(mutationTurnId).some((text) => text.trim() === 'FACTORYD_STATE_WRITTEN'),
    'mutation turn did not finish with the required marker',
  );
  const storedBeforeRestart = await loadFactorydRecord(threadId);
  assert.equal(storedBeforeRestart.threadId, threadId);
  assert.equal(storedBeforeRestart.revision, 4);
  assert.deepStrictEqual(storedBeforeRestart.state, factorydDocumentExpected);
  await assertWorkspaceUnchanged('durable Factory state mutation');

  await client.close();
  client = undefined;

  client = await connectRuntime();
  const resumed = await client.resumeThread({ threadId }, attempt(3));
  assert.equal(resumed.response.thread.id, threadId);
  assert.ok(
    JSON.stringify(resumed.response.raw).includes('FACTORYD_STATE_WRITTEN'),
    'fresh runtime did not resume the exact prior Codex thread history',
  );

  const readTurn = await client.startTurn({
    threadId,
    mode: 'normal',
    model,
    input: [{
      type: 'text',
      text: [
        'Do not call any tools.',
        'Read the authoritative <factory_state> object supplied in native developer context.',
        'Return that entire JSON object exactly, including source, thread_id, durability, and state.',
        'Return JSON only with no Markdown fence or commentary.',
      ].join('\n'),
    }],
  }, attempt(4));
  const readTurnId = readTurn.response.turn.id;
  const readTerminal = await waitForEvent(
    (event) => event.type === 'turnCompleted' && event.turn.id === readTurnId,
    'the fresh-runtime Factory context-read turn to complete',
  );
  assert.equal(readTerminal.turn.status, 'completed');
  assert.deepStrictEqual(
    await rolloutItems(readTurnId, 'function_call'),
    [],
    'fresh-runtime context-read turn unexpectedly called a tool',
  );

  const expectedContext = {
    source: 'factory-native-extension',
    thread_id: threadId,
    durability: 'durable',
    state: stateExpected,
  };
  const injectedContext = factoryContextFromMessages(await rolloutItems(readTurnId, 'message'));
  assert.deepStrictEqual(injectedContext, expectedContext);
  const readMessages = completedMessages(readTurnId);
  assert.ok(readMessages.length > 0, 'fresh-runtime context-read turn produced no agent message');
  assert.deepStrictEqual(JSON.parse(readMessages.at(-1).trim()), expectedContext);

  const storedAfterRestart = await loadFactorydRecord(threadId);
  assert.equal(storedAfterRestart.revision, 4, 'read-only resumed turn unexpectedly saved state');
  assert.deepStrictEqual(storedAfterRestart.state, factorydDocumentExpected);
  await assertWorkspaceUnchanged('full factory-runtime restart and durable state reload');

  succeeded = true;
  console.log(JSON.stringify({
    ok: true,
    model,
    providerBaseUrl,
    factorydUrl,
    modelCatalogJson,
    threadId,
    mutationTurnId,
    readTurnId,
    toolCalls: functionCalls.map((call) => call.name),
    receipts,
    factorydRecordRevision: storedAfterRestart.revision,
    factoryStateRevision: injectedContext.state.revision,
    durability: injectedContext.durability,
    exactInjectedState: true,
    exactModelReportedState: true,
    mutationCallsAfterRestart: 0,
    runtimeRestarted: true,
    workspaceUnchanged: true,
  }));
} finally {
  if (client) await client.close().catch(() => undefined);
  if (succeeded || process.env.FACTORY_KEEP_SMOKE_DIR !== '1') {
    await rm(temporaryRoot, { recursive: true, force: true });
  } else {
    console.error(`preserved failed smoke workspace at ${temporaryRoot}`);
  }
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
        records.push(...lines.map((line) => JSON.parse(line)));
      }
    }
  }
  await visit(root);
  return records;
}
