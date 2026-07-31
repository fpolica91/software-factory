import { access, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
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
const providerBaseUrl = process.env.FACTORY_PROVIDER_BASE_URL ?? 'http://127.0.0.1:10101/v1';
const model = process.env.FACTORY_ZAI_MODEL ?? 'glm-5.2';
const timeoutMs = Number(process.env.FACTORY_SMOKE_TIMEOUT_MS ?? '300000');

await access(runtimePath);
if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
  throw new Error('FACTORY_SMOKE_TIMEOUT_MS must be a positive number');
}

const temporaryRoot = await mkdtemp(resolve(tmpdir(), 'factory-glm-tool-smoke-'));
const codexHome = resolve(temporaryRoot, 'codex-home');
const workspace = resolve(temporaryRoot, 'workspace');
const sourcePath = resolve(workspace, 'source-nonce.txt');
const proofPath = resolve(workspace, 'harness-proof.txt');
const resumedProofPath = resolve(workspace, 'resumed-proof.txt');
await mkdir(codexHome, { recursive: true });
await mkdir(workspace, { recursive: true });
const nonce = `GLM_HARNESS_${randomUUID()}`;
await writeFile(sourcePath, `${nonce}\n`);

const providerConfig = {
  name: 'Software Factory provider bridge',
  base_url: providerBaseUrl,
  wire_api: 'responses',
  requires_openai_auth: false,
  supports_websockets: false,
};
const configToml = [
  `model = ${JSON.stringify(model)}`,
  'model_provider = "factory-provider"',
  'model_context_window = 1000000',
  'model_reasoning_effort = "medium"',
  'approval_policy = "never"',
  'sandbox_mode = "danger-full-access"',
  '',
  '[model_providers.factory-provider]',
  `name = ${JSON.stringify(providerConfig.name)}`,
  `base_url = ${JSON.stringify(providerConfig.base_url)}`,
  `wire_api = ${JSON.stringify(providerConfig.wire_api)}`,
  `requires_openai_auth = ${String(providerConfig.requires_openai_auth)}`,
  `supports_websockets = ${String(providerConfig.supports_websockets)}`,
  '',
].join('\n');
await writeFile(resolve(codexHome, 'config.toml'), configToml, { mode: 0o600 });

const seed = {
  jobId: 'glm-functional-proof',
  operationId: 'codex-tool-compaction-resume',
  workflowRunId: 'glm-functional-proof-run',
  taskRunExternalId: 'glm-functional-proof-task',
};
const observedEvents = [];
const terminalErrors = [];
let client;
let succeeded = false;
const trace = process.env.FACTORY_SMOKE_TRACE === '1';

function attempt(number) {
  return { ...seed, attemptId: `glm-attempt-${number}` };
}

function eventSummary({ event, correlation }) {
  return {
    type: event.type,
    itemType: 'item' in event ? event.item.type : undefined,
    threadId: 'threadId' in event ? event.threadId : undefined,
    turnId: 'turnId' in event ? event.turnId : ('turn' in event ? event.turn.id : undefined),
    attemptId: correlation?.attemptId,
  };
}

function itemSummary(item) {
  if (item.type === 'agentMessage') return { type: item.type, text: item.text };
  if (item.type === 'commandExecution') {
    return {
      type: item.type,
      command: item.command,
      status: item.status,
      exitCode: item.exitCode,
      output: item.aggregatedOutput,
    };
  }
  if (item.type === 'fileChange') return { type: item.type, status: item.status, changes: item.changes };
  if (item.type === 'reasoning') return { type: item.type, summary: item.summary };
  return { type: item.type };
}

async function waitForEvent(predicate, description) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const match = observedEvents.find(({ event }) => predicate(event));
    if (match) return match.event;
    if (terminalErrors.length > 0) {
      throw new Error(`factory runtime failed while waiting for ${description}: ${terminalErrors.at(-1).message}`);
    }
    const runtimeFailure = observedEvents.find(({ event }) => event.type === 'runtimeError');
    if (runtimeFailure) {
      throw new Error(`runtime failed while waiting for ${description}: ${JSON.stringify(runtimeFailure.event.error)}`);
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 50));
  }
  throw new Error(`timed out waiting for ${description}`);
}

function connect() {
  return FactoryClient.connect({
    runtimePath,
    cwd: workspace,
    env: { CODEX_HOME: codexHome },
    onEvent: (correlated) => {
      observedEvents.push(correlated);
      if (trace && correlated.event.type !== 'itemDelta') {
        const detail = 'item' in correlated.event
          ? { ...eventSummary(correlated), item: itemSummary(correlated.event.item) }
          : eventSummary(correlated);
        console.error(JSON.stringify(detail));
      }
    },
    onError: (error) => terminalErrors.push(error),
  });
}

try {
  client = await connect();
  const started = await client.startThread({
    model,
    modelProvider: 'factory-provider',
    cwd: workspace,
    approvalPolicy: 'never',
    sandbox: 'danger-full-access',
    config: {
      'model_providers.factory-provider': providerConfig,
      model_context_window: 1_000_000,
      model_reasoning_effort: 'medium',
    },
  }, attempt(1));
  const threadId = started.response.thread.id;

  const first = await client.startTurn({
    threadId,
    mode: 'normal',
    input: [{
      type: 'text',
      text: [
        'This is a functional harness proof. Perform these actions; do not merely describe them:',
        '1. Use a shell command to read source-nonce.txt. Its value is intentionally absent from this prompt.',
        '2. Use a separate shell command to write that exact value to harness-proof.txt followed by a newline.',
        '3. Use another shell command to read harness-proof.txt and verify the copied value.',
        '4. After the tool results, finish with FIRST_TURN_COMPLETE followed by the exact value you observed.',
      ].join('\n'),
    }],
  }, attempt(2));
  const firstTurnId = first.response.turn.id;
  const completedFirstTurn = await waitForEvent(
    (event) => event.type === 'turnCompleted' && event.turn.id === firstTurnId,
    'the first GLM turn to complete',
  );
  if (completedFirstTurn.type !== 'turnCompleted' || completedFirstTurn.turn.status !== 'completed') {
    throw new Error(`first GLM turn ended with status ${completedFirstTurn.type === 'turnCompleted' ? completedFirstTurn.turn.status : 'unknown'}`);
  }
  const firstTurnItems = observedEvents
    .filter(({ event }) => 'turnId' in event && event.turnId === firstTurnId && 'item' in event)
    .map(({ event }) => event.item);
  let proof;
  try {
    proof = await readFile(proofPath, 'utf8');
  } catch (error) {
    throw new Error(`first GLM turn did not create harness-proof.txt; items: ${JSON.stringify(firstTurnItems.map(itemSummary))}`, { cause: error });
  }
  if (proof !== `${nonce}\n`) {
    throw new Error(`GLM tool turn wrote unexpected proof content: ${JSON.stringify(proof)}; items: ${JSON.stringify(firstTurnItems.map(itemSummary))}`);
  }
  const successfulFirstCommands = firstTurnItems.filter((item) =>
    item.type === 'commandExecution' && item.status === 'completed' && item.exitCode === 0,
  );
  if (successfulFirstCommands.length < 2) {
    throw new Error('first GLM turn completed without multiple successful command-tool items');
  }
  if (!firstTurnItems.some((item) =>
    item.type === 'agentMessage' && item.text.includes('FIRST_TURN_COMPLETE') && item.text.includes(nonce),
  )) {
    throw new Error('first GLM turn did not continue from tool outputs to the required model response');
  }

  await client.compactThread({ threadId }, attempt(3));
  const compactionStarted = await waitForEvent(
    (event) => event.type === 'itemStarted' && event.threadId === threadId && event.item.type === 'contextCompaction',
    'Codex context compaction to start',
  );
  if (compactionStarted.type !== 'itemStarted') throw new Error('unexpected compaction event');
  await waitForEvent(
    (event) => event.type === 'itemCompleted' &&
      event.threadId === threadId &&
      event.item.type === 'contextCompaction' &&
      event.item.id === compactionStarted.item.id,
    'Codex context compaction to complete',
  );
  await client.close();
  client = undefined;
  await rm(sourcePath, { force: true });
  await rm(proofPath, { force: true });

  client = await connect();
  const resumed = await client.resumeThread({ threadId }, attempt(4));
  if (resumed.response.thread.id !== threadId) {
    throw new Error('resumed runtime returned a different thread id');
  }

  const second = await client.startTurn({
    threadId,
    mode: 'normal',
    input: [{
      type: 'text',
      text: [
        'Confirm that the compacted, resumed thread retained the value from the prior turn.',
        'The original files are gone. Use a shell command to write the exact remembered value to resumed-proof.txt followed by a newline.',
        'Use a separate shell command to read resumed-proof.txt.',
        'If and only if it matches the prior value, finish with RESUME_COMPLETE followed by that exact value.',
      ].join('\n'),
    }],
  }, attempt(5));
  const secondTurnId = second.response.turn.id;
  await waitForEvent(
    (event) => event.type === 'turnCompleted' && event.turn.id === secondTurnId,
    'the post-resume GLM turn to complete',
  );
  const secondTurnItems = observedEvents
    .filter(({ event }) => 'turnId' in event && event.turnId === secondTurnId && 'item' in event)
    .map(({ event }) => event.item);
  const resumedProof = await readFile(resumedProofPath, 'utf8');
  if (resumedProof !== `${nonce}\n`) {
    throw new Error(`resumed turn wrote unexpected remembered value: ${JSON.stringify(resumedProof)}`);
  }
  if (!secondTurnItems.some((item) =>
    item.type === 'commandExecution' && item.status === 'completed' && item.exitCode === 0,
  )) {
    throw new Error('resumed GLM turn completed without a successful Codex command tool');
  }
  if (!secondTurnItems.some((item) =>
    item.type === 'agentMessage' && item.text.includes('RESUME_COMPLETE') && item.text.includes(nonce),
  )) {
    throw new Error('resumed GLM turn did not continue from the tool output to the required response');
  }

  succeeded = true;
  console.log(JSON.stringify({
    ok: true,
    model,
    providerBaseUrl,
    threadId,
    firstTurnId,
    secondTurnId,
    proof: 'hidden nonce copied, compacted, recalled, and verified',
    lifecycle: [
      'thread/start',
      'turn/start',
      'shell/read-hidden-value',
      'shell/copy-and-verify',
      'thread/compact/start',
      'runtime/restart',
      'thread/resume',
      'turn/start',
      'shell/recreate-and-verify-from-memory',
    ],
    events: observedEvents
      .filter(({ event }) => event.type !== 'itemDelta' && event.type !== 'unknownNotification')
      .map(eventSummary),
  }));
} finally {
  if (client) await client.close().catch(() => undefined);
  if (succeeded || process.env.FACTORY_KEEP_SMOKE_DIR !== '1') {
    await rm(temporaryRoot, { recursive: true, force: true });
  } else {
    console.error(`preserved failed smoke workspace at ${temporaryRoot}`);
  }
}
