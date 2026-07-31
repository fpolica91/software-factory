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
import { dirname, relative, resolve } from 'node:path';
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
const providerBaseUrl = process.env.FACTORY_PROVIDER_BASE_URL ?? 'http://127.0.0.1:18101/v1';
const model = process.env.FACTORY_ZAI_MODEL ?? 'glm-5.2';
const timeoutMs = Number(process.env.FACTORY_SMOKE_TIMEOUT_MS ?? '300000');

await access(runtimePath);
if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
  throw new Error('FACTORY_SMOKE_TIMEOUT_MS must be a positive number');
}

const temporaryRoot = await mkdtemp(resolve(tmpdir(), 'factory-glm-plan-smoke-'));
const codexHome = resolve(temporaryRoot, 'codex-home');
const workspace = resolve(temporaryRoot, 'workspace');
await mkdir(codexHome, { recursive: true });
await mkdir(resolve(workspace, 'src'), { recursive: true });
await writeFile(resolve(workspace, 'README.md'), [
  '# Durable job runner',
  '',
  'The runner executes named stages in order. Interrupted jobs currently restart from stage one.',
  '',
].join('\n'));
await writeFile(resolve(workspace, 'src', 'runner.ts'), [
  'export async function run(stages: Array<() => Promise<void>>): Promise<void> {',
  '  for (const stage of stages) await stage();',
  '}',
  '',
].join('\n'));

const providerConfig = {
  name: 'Software Factory provider bridge',
  base_url: providerBaseUrl,
  wire_api: 'responses',
  requires_openai_auth: false,
  supports_websockets: false,
};
await writeFile(resolve(codexHome, 'config.toml'), [
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
].join('\n'));

const baseline = await snapshotTree(workspace);
const runId = randomUUID();
const seed = {
  jobId: 'glm-plan-functional-proof',
  operationId: 'codex-plan-restart-resume',
  workflowRunId: `glm-plan-run-${runId}`,
  taskRunExternalId: `glm-plan-task-${runId}`,
};
const trace = process.env.FACTORY_SMOKE_TRACE === '1';
let client;
let succeeded = false;

function attempt(number) {
  return { ...seed, attemptId: `glm-plan-attempt-${number}` };
}

async function assertWorkspaceUnchanged(stage) {
  const current = await snapshotTree(workspace);
  if (JSON.stringify(current) !== JSON.stringify(baseline)) {
    throw new Error(`workspace changed during ${stage}: ${describeTreeDifference(baseline, current)}`);
  }
}

async function connectRuntime() {
  const events = [];
  const errors = [];
  const connected = await FactoryClient.connect({
    runtimePath,
    cwd: workspace,
    env: { CODEX_HOME: codexHome },
    onEvent: (correlated) => {
      events.push(correlated);
      if (trace && correlated.event.type !== 'itemDelta') {
        console.error(JSON.stringify({
          type: correlated.event.type,
          itemType: 'item' in correlated.event ? correlated.event.item.type : undefined,
          threadId: 'threadId' in correlated.event ? correlated.event.threadId : undefined,
          turnId: 'turnId' in correlated.event
            ? correlated.event.turnId
            : ('turn' in correlated.event ? correlated.event.turn.id : undefined),
        }));
      }
    },
    onError: (error) => errors.push(error),
  });
  return { client: connected, events, errors };
}

async function waitForEvent(runtime, predicate, description) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const match = runtime.events.find(({ event }) => predicate(event));
    if (match) return match.event;
    const failure = runtime.events.find(({ event }) => event.type === 'runtimeError');
    if (failure) {
      throw new Error(`runtime failed while waiting for ${description}: ${JSON.stringify(failure.event.error)}`);
    }
    if (runtime.errors.length > 0) {
      throw new Error(`runtime connection failed while waiting for ${description}: ${runtime.errors.at(-1).message}`);
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 50));
  }
  throw new Error(`timed out waiting for ${description}`);
}

function isActionablePlan(plan) {
  return plan.length >= 3 && plan.every(({ step, status }) =>
    typeof step === 'string' &&
    step.trim().length >= 12 &&
    step.trim().includes(' ') &&
    ['pending', 'inProgress', 'completed'].includes(status),
  );
}

function rawContainsExactText(raw, expected) {
  const serialized = JSON.stringify(raw);
  return serialized.includes(JSON.stringify(expected).slice(1, -1));
}

function isSubstantivePlanText(text) {
  const lines = text.split('\n').map((line) => line.trim()).filter(Boolean);
  const actionableLines = lines.filter((line) => /^(?:[-*]|\d+[.)])\s+\S/.test(line));
  return text.trim().length >= 200 && lines.length >= 6 && actionableLines.length >= 3;
}

function normalizePlanText(text) {
  return text
    .toLowerCase()
    .replace(/[`*_#<>]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
}

function checklistCopiesPriorPlan(checklist, planText) {
  const normalizedPlan = normalizePlanText(planText);
  return checklist.length >= 3 && checklist.every(({ step }) =>
    normalizedPlan.includes(normalizePlanText(step)),
  );
}

try {
  const firstRuntime = await connectRuntime();
  client = firstRuntime.client;
  const started = await client.startThread({
    model,
    modelProvider: 'factory-provider',
    cwd: workspace,
    developerInstructions: [
      'Planning turns are read-only: do not create, edit, delete, or rename workspace files.',
      'In Plan mode, return the substantive implementation plan inside exactly one <proposed_plan>...</proposed_plan> block so Codex emits its completed plan item; do not use the update_plan checklist tool.',
      'In Normal mode, use update_plan when the user explicitly asks for the checklist mechanism.',
    ].join(' '),
  }, attempt(1));
  const threadId = started.response.thread.id;
  if (started.response.modelProvider !== 'factory-provider' || started.response.model !== model) {
    throw new Error(`thread did not select ${model} through factory-provider`);
  }

  const turnStarted = await client.startTurn({
    threadId,
    mode: 'plan',
    model,
    input: [{
      type: 'text',
      text: [
        'Inspect the small repository if useful, then produce an implementation plan only.',
        'Plan how to add durable stage checkpoints so an interrupted job resumes after its last completed stage.',
        'Return at least three concrete, actionable plan items covering implementation and functional verification.',
        'Your entire final answer must be one <proposed_plan>...</proposed_plan> block with no text before or after it.',
        'Do not implement the plan and do not modify the workspace.',
      ].join('\n'),
    }],
  }, attempt(2));
  const planTurnId = turnStarted.response.turn.id;
  const terminal = await waitForEvent(
    firstRuntime,
    (event) => event.type === 'turnCompleted' && event.turn.id === planTurnId,
    'the GLM plan turn to complete',
  );
  if (terminal.type !== 'turnCompleted' || terminal.turn.status !== 'completed') {
    throw new Error(`plan turn ended with status ${terminal.type === 'turnCompleted' ? terminal.turn.status : 'unknown'}`);
  }
  const completedPlanItems = firstRuntime.events
    .map(({ event }) => event)
    .filter((event) =>
      event.type === 'itemCompleted' &&
      event.turnId === planTurnId &&
      event.item.type === 'plan' &&
      isSubstantivePlanText(event.item.text),
    );
  if (completedPlanItems.length === 0) {
    const itemTypes = firstRuntime.events
      .map(({ event }) => 'item' in event ? event.item.type : undefined)
      .filter(Boolean);
    throw new Error(`GLM plan turn completed without a substantive completed plan item; observed items: ${itemTypes.join(', ')}`);
  }
  const authoredPlanText = completedPlanItems.at(-1).item.text;
  await assertWorkspaceUnchanged('the plan turn');

  await client.close();
  client = undefined;

  const resumedRuntime = await connectRuntime();
  client = resumedRuntime.client;
  const resumed = await client.resumeThread({ threadId }, attempt(3));
  if (resumed.response.thread.id !== threadId) {
    throw new Error('fresh runtime returned a different thread id on resume');
  }
  if (!rawContainsExactText(resumed.response.raw, authoredPlanText)) {
    throw new Error('thread/resume raw history did not contain the exact completed plan item');
  }

  const contextTurn = await client.startTurn({
    threadId,
    mode: 'normal',
    input: [{
      type: 'text',
      text: [
        'Use the native update_plan checklist tool now.',
        'From conversation context only, copy at least three implementation actions verbatim from your immediately preceding completed plan into the plan step fields.',
        'Do not create a new plan, paraphrase, inspect the workspace, or modify files.',
        'Set one copied step in_progress and the remaining copied steps pending.',
        'After the tool succeeds, respond with exactly CONTEXT_PLAN_RESTORED.',
      ].join('\n'),
    }],
  }, attempt(4));
  const contextTurnId = contextTurn.response.turn.id;
  const contextTerminal = await waitForEvent(
    resumedRuntime,
    (event) => event.type === 'turnCompleted' && event.turn.id === contextTurnId,
    'the post-resume context turn to complete',
  );
  if (contextTerminal.type !== 'turnCompleted' || contextTerminal.turn.status !== 'completed') {
    throw new Error('post-resume context turn did not complete');
  }
  const restoredPlanEvents = resumedRuntime.events
    .map(({ event }) => event)
    .filter((event) =>
      event.type === 'turnPlanUpdated' &&
      event.turnId === contextTurnId &&
      isActionablePlan(event.plan) &&
      checklistCopiesPriorPlan(event.plan, authoredPlanText),
    );
  if (restoredPlanEvents.length === 0) {
    throw new Error('resumed normal-mode turn did not publish a native checklist copied from the completed plan');
  }
  await assertWorkspaceUnchanged('runtime restart and thread resume');

  succeeded = true;
  console.log(JSON.stringify({
    ok: true,
    model,
    providerBaseUrl,
    threadId,
    planTurnId,
    terminalStatus: terminal.turn.status,
    completedPlanItems: completedPlanItems.length,
    completedPlanCharacters: authoredPlanText.length,
    resumedChecklistTurnId: contextTurnId,
    resumedChecklist: restoredPlanEvents.at(-1).plan,
    workspaceUnchanged: true,
    runtimeRestarted: true,
    persistence: 'exact completed plan item in thread/resume raw history',
    resumedContext: 'native turnPlanUpdated copied verbatim from prior plan',
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

function describeTreeDifference(before, after) {
  const beforeByPath = new Map(before.map((entry) => [entry.path, entry]));
  const afterByPath = new Map(after.map((entry) => [entry.path, entry]));
  const changed = [...new Set([...beforeByPath.keys(), ...afterByPath.keys()])]
    .filter((path) => JSON.stringify(beforeByPath.get(path)) !== JSON.stringify(afterByPath.get(path)));
  return changed.join(', ') || 'unknown difference';
}
