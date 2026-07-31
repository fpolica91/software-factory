import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import {
  access,
  mkdir,
  mkdtemp,
  readdir,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

import { FactoryClient } from '../dist/index.js';

const run = promisify(execFile);
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
const mcpFixturePath = resolve(packageRoot, 'scripts', 'fixtures', 'kernel-parity-mcp.mjs');
const providerBaseUrl = process.env.FACTORY_PROVIDER_BASE_URL ?? 'http://127.0.0.1:18101/v1';
const modelCatalogJson = process.env.FACTORY_MODEL_CATALOG_JSON ??
  '/tmp/software-factory-provider-glm52/codex-models.json';
const model = process.env.FACTORY_ZAI_MODEL ?? 'glm-5.2';
const timeoutMs = Number(process.env.FACTORY_KERNEL_PARITY_TIMEOUT_MS ?? '600000');

await access(runtimePath);
await access(mcpFixturePath);
if (!isAbsolute(modelCatalogJson)) {
  throw new Error('FACTORY_MODEL_CATALOG_JSON must be an absolute path');
}
await access(modelCatalogJson);
if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
  throw new Error('FACTORY_KERNEL_PARITY_TIMEOUT_MS must be a positive number');
}

const temporaryRoot = await mkdtemp(resolve(tmpdir(), 'factory-kernel-parity-'));
const codexHome = resolve(temporaryRoot, 'codex-home');
const workspace = resolve(temporaryRoot, 'workspace');
const lifecyclePath = resolve(temporaryRoot, 'mcp-lifecycle.jsonl');
const configPath = resolve(codexHome, 'config.toml');
const fixtureSkillPath = resolve(codexHome, 'skills', 'kernel-parity', 'SKILL.md');
const notifications = [];
const clientErrors = [];
let client;

function knownNotifications(method) {
  return notifications
    .filter((entry) => entry.kind === 'known' && entry.notification.method === method)
    .map((entry) => entry.notification.params);
}

async function waitFor(description, predicate, timeout = timeoutMs) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const value = predicate();
    if (value) return value;
    if (clientErrors.length > 0) throw clientErrors.at(-1);
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 50));
  }
  throw new Error(`timed out waiting for ${description}`);
}

async function waitForTurn(threadId, turnId, description) {
  const completed = await waitFor(description, () => knownNotifications('turn/completed')
    .find((params) => params.threadId === threadId && params.turn.id === turnId));
  assert.equal(completed.turn.status, 'completed', `${description} did not complete successfully`);
  return completed.turn;
}

async function startTurn(threadId, text, description) {
  const response = await client.requestCodex('turn/start', {
    threadId,
    input: [{ type: 'text', text, text_elements: [] }],
    model,
    effort: 'low',
  });
  return waitForTurn(threadId, response.turn.id, description);
}

function mcpStatus(response) {
  const status = response.data.find((entry) => entry.name === 'kernel_parity');
  assert.ok(status, 'MCP inventory did not contain kernel_parity');
  return status;
}

function completedItems(threadId) {
  return knownNotifications('item/completed')
    .filter((params) => params.threadId === threadId)
    .map((params) => params.item);
}

async function rolloutFunctionCalls(codexRoot, turnId) {
  const files = [];
  async function visit(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true }).catch(() => [])) {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) await visit(path);
      else if (entry.isFile() && entry.name.endsWith('.jsonl')) files.push(path);
    }
  }
  await visit(resolve(codexRoot, 'sessions'));
  const calls = [];
  for (const path of files) {
    const records = (await readFile(path, 'utf8')).trim().split('\n').filter(Boolean);
    for (const record of records) {
      const entry = JSON.parse(record);
      const item = entry.type === 'response_item' ? entry.payload : undefined;
      if (item?.type !== 'function_call') continue;
      if (item.internal_chat_message_metadata_passthrough?.turn_id !== turnId) continue;
      calls.push(item);
    }
  }
  return calls;
}

async function collaborationFunctionCalls(codexRoot, turnId) {
  const currentVocabulary = new Set([
    'list_agents',
    'spawn_agent',
    'send_message',
    'wait_agent',
    'followup_task',
    'interrupt_agent',
  ]);
  return (await rolloutFunctionCalls(codexRoot, turnId))
    .filter((item) => item.type === 'function_call' && (
      item.namespace === 'collaboration' || currentVocabulary.has(item.name)
    ))
    .map((item) => item.name);
}

function assertCollaborationVocabulary(actual) {
  for (const tool of [
    'spawn_agent',
    'send_message',
    'followup_task',
    'interrupt_agent',
  ]) {
    assert.ok(actual.includes(tool), `collaboration did not call ${tool}: ${JSON.stringify(actual)}`);
  }
  assert.ok(actual.filter((tool) => tool === 'wait_agent').length >= 2,
    `collaboration did not call wait_agent twice: ${JSON.stringify(actual)}`);
  assert.ok(actual.filter((tool) => tool === 'list_agents').length >= 2,
    `collaboration did not call list_agents twice: ${JSON.stringify(actual)}`);
}

try {
  await mkdir(dirname(fixtureSkillPath), { recursive: true });
  await mkdir(workspace, { recursive: true });
  await writeFile(fixtureSkillPath, [
    '---',
    'name: kernel-parity',
    'description: Disposable skill used by the Codex kernel parity acceptance.',
    '---',
    '',
    '# Kernel parity',
    '',
    'KERNEL_PARITY_SKILL_MARKER',
    '',
  ].join('\n'));
  await writeFile(lifecyclePath, '');

  await writeFile(resolve(workspace, 'calculator.js'), [
    'export function add(left, right) {',
    '  return left + right;',
    '}',
    '',
  ].join('\n'));
  await run('git', ['init', '-b', 'main'], { cwd: workspace });
  await run('git', ['config', 'user.name', 'Kernel Parity Fixture'], { cwd: workspace });
  await run('git', ['config', 'user.email', 'kernel-parity@example.invalid'], { cwd: workspace });
  await run('git', ['add', 'calculator.js'], { cwd: workspace });
  await run('git', ['commit', '-m', 'fixture: establish correct addition'], { cwd: workspace });
  await writeFile(resolve(workspace, 'calculator.js'), [
    'export function add(left, right) {',
    '  return left - right;',
    '}',
    '',
  ].join('\n'));
  const expectedDiff = (await run('git', ['diff', '--', 'calculator.js'], { cwd: workspace })).stdout;
  assert.match(expectedDiff, /return left - right;/, 'review fixture change is missing');

  await mkdir(codexHome, { recursive: true });
  await writeFile(configPath, [
    `model = ${JSON.stringify(model)}`,
    'model_provider = "factory-provider"',
    'model_context_window = 1000000',
    'model_reasoning_effort = "low"',
    'approval_policy = "never"',
    'sandbox_mode = "danger-full-access"',
    `model_catalog_json = ${JSON.stringify(modelCatalogJson)}`,
    '',
    '[features]',
    'goals = true',
    'multi_agent = true',
    '',
    '[features.multi_agent_v2]',
    'enabled = true',
    'wait_agent_enabled = true',
    '',
    '[model_providers.factory-provider]',
    'name = "Software Factory provider bridge"',
    `base_url = ${JSON.stringify(providerBaseUrl)}`,
    'wire_api = "responses"',
    'requires_openai_auth = false',
    'supports_websockets = false',
    '',
    '[mcp_servers.kernel_parity]',
    `command = ${JSON.stringify(process.execPath)}`,
    `args = [${JSON.stringify(mcpFixturePath)}]`,
    'startup_timeout_sec = 10',
    'tool_timeout_sec = 20',
    '',
    '[mcp_servers.kernel_parity.env]',
    'KERNEL_PARITY_MCP_GENERATION = "generation-one"',
    `KERNEL_PARITY_MCP_LIFECYCLE_FILE = ${JSON.stringify(lifecyclePath)}`,
    '',
  ].join('\n'), { mode: 0o600 });

  client = await FactoryClient.connect({
    runtimePath,
    cwd: workspace,
    env: { CODEX_HOME: codexHome },
    onCodexNotification: (notification) => notifications.push(notification),
    onError: (error) => clientErrors.push(error),
  });

  const skills = await client.requestCodex('skills/list', {
    cwds: [workspace],
    forceReload: true,
  });
  const skillEntry = skills.data.find((entry) => entry.cwd === workspace) ?? skills.data[0];
  assert.ok(skillEntry, 'skills/list returned no entry for the fixture workspace');
  const fixtureSkill = skillEntry.skills.find((skill) => skill.name === 'kernel-parity');
  assert.ok(fixtureSkill, 'skills/list did not discover the disposable fixture skill');
  assert.equal(fixtureSkill.path, fixtureSkillPath);
  assert.equal(fixtureSkill.enabled, true);

  const rootStart = await client.requestCodex('thread/start', {
    cwd: workspace,
    model,
    approvalPolicy: 'never',
    sandbox: 'danger-full-access',
  });
  const rootThreadId = rootStart.thread.id;

  const inventoryOne = mcpStatus(await client.requestCodex('mcpServerStatus/list', {
    detail: 'full',
    threadId: rootThreadId,
    limit: 20,
  }));
  assert.equal(inventoryOne.serverInfo?.version, 'generation-one');
  assert.ok(inventoryOne.resources.some((resource) => resource.uri === 'fixture://kernel-parity/state'));
  assert.ok(inventoryOne.tools.echo_generation, 'MCP inventory did not expose echo_generation');

  const resourceOne = await client.requestCodex('mcpServer/resource/read', {
    threadId: rootThreadId,
    server: 'kernel_parity',
    uri: 'fixture://kernel-parity/state',
  });
  assert.equal(resourceOne.contents[0]?.text, 'kernel-parity-resource:generation-one');

  const toolOne = await client.requestCodex('mcpServer/tool/call', {
    threadId: rootThreadId,
    server: 'kernel_parity',
    tool: 'echo_generation',
    arguments: { message: 'before-reload' },
  });
  assert.deepEqual(toolOne.structuredContent, {
    echoed: 'before-reload',
    generation: 'generation-one',
  });

  const generationOneConfig = await readFile(configPath, 'utf8');
  const generationTwoConfig = generationOneConfig.replace(
    'KERNEL_PARITY_MCP_GENERATION = "generation-one"',
    'KERNEL_PARITY_MCP_GENERATION = "generation-two"',
  );
  assert.notEqual(generationTwoConfig, generationOneConfig, 'MCP reload fixture config did not change');
  await writeFile(configPath, generationTwoConfig, { mode: 0o600 });
  await client.requestCodex('config/mcpServer/reload');
  const inventoryTwo = mcpStatus(await client.requestCodex('mcpServerStatus/list', {
    detail: 'full',
    threadId: rootThreadId,
    limit: 20,
  }));
  assert.equal(inventoryTwo.serverInfo?.version, 'generation-two');
  const resourceTwo = await client.requestCodex('mcpServer/resource/read', {
    threadId: rootThreadId,
    server: 'kernel_parity',
    uri: 'fixture://kernel-parity/state',
  });
  assert.equal(resourceTwo.contents[0]?.text, 'kernel-parity-resource:generation-two');
  const toolTwo = await client.requestCodex('mcpServer/tool/call', {
    threadId: rootThreadId,
    server: 'kernel_parity',
    tool: 'echo_generation',
    arguments: { message: 'after-reload' },
  });
  assert.deepEqual(toolTwo.structuredContent, {
    echoed: 'after-reload',
    generation: 'generation-two',
  });
  const lifecycle = (await readFile(lifecyclePath, 'utf8'))
    .trim()
    .split('\n')
    .filter(Boolean)
    .map((line) => JSON.parse(line));
  assert.ok(lifecycle.some((entry) => entry.generation === 'generation-one'));
  assert.ok(lifecycle.some((entry) => entry.generation === 'generation-two'));

  await startTurn(
    rootThreadId,
    'Reply exactly ROOT-MATERIALIZED. Do not call tools.',
    'root materialization turn',
  );
  const objective = 'Prove the disposable kernel parity goal lifecycle.';
  const goalSet = await client.requestCodex('thread/goal/set', {
    threadId: rootThreadId,
    objective,
    status: 'paused',
  });
  assert.equal(goalSet.goal.threadId, rootThreadId);
  assert.equal(goalSet.goal.objective, objective);
  assert.equal(goalSet.goal.status, 'paused');
  const goalUpdated = await waitFor('thread/goal/updated notification', () =>
    knownNotifications('thread/goal/updated')
      .find((params) => params.threadId === rootThreadId && params.goal.objective === objective));
  assert.equal(goalUpdated.goal.objective, objective);
  const goalGet = await client.requestCodex('thread/goal/get', { threadId: rootThreadId });
  assert.equal(goalGet.goal?.objective, objective);
  const goalClear = await client.requestCodex('thread/goal/clear', { threadId: rootThreadId });
  assert.equal(goalClear.cleared, true);
  await waitFor('thread/goal/cleared notification', () =>
    knownNotifications('thread/goal/cleared').find((params) => params.threadId === rootThreadId));
  const goalAfterClear = await client.requestCodex('thread/goal/get', { threadId: rootThreadId });
  assert.equal(goalAfterClear.goal, null);

  const reviewStart = await client.requestCodex('thread/start', {
    cwd: workspace,
    model,
    approvalPolicy: 'never',
    sandbox: 'danger-full-access',
  });
  const reviewResponse = await client.requestCodex('review/start', {
    threadId: reviewStart.thread.id,
    delivery: 'inline',
    target: { type: 'uncommittedChanges' },
  });
  assert.equal(reviewResponse.reviewThreadId, reviewStart.thread.id);
  await waitForTurn(
    reviewStart.thread.id,
    reviewResponse.turn.id,
    'review/start turn completion',
  );
  const enteredReview = knownNotifications('item/started').find((params) =>
    params.threadId === reviewStart.thread.id &&
      params.turnId === reviewResponse.turn.id &&
      params.item.type === 'enteredReviewMode');
  assert.ok(enteredReview, 'review/start did not emit enteredReviewMode');
  const exitedReview = knownNotifications('item/completed').find((params) =>
    params.threadId === reviewStart.thread.id &&
      params.turnId === reviewResponse.turn.id &&
      params.item.type === 'exitedReviewMode');
  assert.ok(exitedReview, 'review/start did not emit exitedReviewMode');
  assert.equal(
    (await run('git', ['diff', '--', 'calculator.js'], { cwd: workspace })).stdout,
    expectedDiff,
    'review/start changed the disposable fixture',
  );

  const collaborationPrompt = [
    'Run this exact native Codex collaboration lifecycle. Do not call shell, MCP, Factory, or file tools.',
    'Use only the currently advertised collaboration namespace.',
    '1. Call collaboration.list_agents.',
    '2. Call collaboration.spawn_agent exactly once with task_name parity_child, fork_turns none, and message: Reply exactly CHILD-INITIAL-READY, then complete.',
    '3. Call collaboration.send_message for /root/parity_child with message: CHILD-QUEUED-MARKER.',
    '4. Call collaboration.wait_agent until the child completion arrives.',
    '5. Call collaboration.followup_task for /root/parity_child with message: Reply exactly CHILD-FOLLOWUP-READY, then complete.',
    '6. Call collaboration.wait_agent again until the follow-up completion arrives.',
    '7. Call collaboration.interrupt_agent for /root/parity_child as terminal cleanup.',
    '8. Call collaboration.list_agents again.',
    'Only after all eight native calls succeed, reply exactly COLLAB-PARITY-COMPLETE.',
  ].join('\n');
  const collabTurn = await startTurn(
    rootThreadId,
    collaborationPrompt,
    'native collaboration lifecycle',
  );
  const collabCalls = await collaborationFunctionCalls(codexHome, collabTurn.id);
  assertCollaborationVocabulary(collabCalls);
  const activityItems = completedItems(rootThreadId)
    .filter((item) => item.type === 'subAgentActivity');
  const startedActivity = activityItems.find((item) => item.kind === 'started');
  assert.ok(startedActivity?.agentThreadId, 'spawn_agent did not expose the child thread id');
  const childThreadId = startedActivity.agentThreadId;
  assert.ok(
    activityItems.filter((item) =>
      item.kind === 'interacted' && item.agentThreadId === childThreadId).length >= 2,
    'send_message and followup_task did not interact with the spawned child',
  );
  assert.ok(
    activityItems.some((item) =>
      item.kind === 'interrupted' && item.agentThreadId === childThreadId),
    'interrupt_agent did not target the spawned child',
  );
  assert.ok(
    completedItems(rootThreadId)
      .filter((item) => item.type === 'collabAgentToolCall' && item.tool === 'wait')
      .length >= 2,
    'wait_agent did not complete twice',
  );

  const childList = await client.requestCodex('thread/list', {
    limit: 100,
    cwd: workspace,
    sourceKinds: [
      'subAgent',
      'subAgentThreadSpawn',
      'subAgentOther',
    ],
  });
  const listedChild = childList.data.find((thread) => thread.id === childThreadId);
  assert.ok(listedChild, 'thread/list did not return the spawned child thread');
  assert.equal(listedChild.parentThreadId, rootThreadId);
  const childRead = await client.requestCodex('thread/read', {
    threadId: childThreadId,
    includeTurns: true,
  });
  assert.equal(childRead.thread.id, childThreadId);
  assert.equal(childRead.thread.parentThreadId, rootThreadId);
  assert.ok(childRead.thread.turns.length >= 2, 'child thread did not persist initial and follow-up turns');
  assert.equal(collabTurn.status, 'completed');
  assert.equal(
    (await run('git', ['diff', '--', 'calculator.js'], { cwd: workspace })).stdout,
    expectedDiff,
    'collaboration changed the disposable fixture',
  );

  await client.close();
  client = undefined;
  console.log(JSON.stringify({
    phase: 'codexKernelParityAccepted',
    model,
    skills: {
      name: fixtureSkill.name,
      scope: fixtureSkill.scope,
    },
    mcp: {
      server: 'kernel_parity',
      inventory: ['echo_generation', 'fixture://kernel-parity/state'],
      generations: ['generation-one', 'generation-two'],
      resourceReads: ['generation-one', 'generation-two'],
      toolCalls: ['before-reload', 'after-reload'],
    },
    goals: {
      lifecycle: ['set', 'get', 'clear', 'get-null'],
      notifications: ['thread/goal/updated', 'thread/goal/cleared'],
    },
    review: {
      target: 'uncommittedChanges',
      threadId: reviewStart.thread.id,
      items: ['enteredReviewMode', 'exitedReviewMode'],
      fixtureUnchanged: true,
    },
    collaboration: {
      namespace: 'collaboration',
      parentThreadId: rootThreadId,
      childThreadId,
      lifecycle: [
        'list_agents',
        'spawn_agent',
        'send_message',
        'wait_agent',
        'followup_task',
        'wait_agent',
        'interrupt_agent',
        'list_agents',
      ],
      childTurns: childRead.thread.turns.length,
      childListed: true,
      childRead: true,
    },
  }));
} finally {
  if (client) await client.close().catch(() => undefined);
  await rm(temporaryRoot, { recursive: true, force: true });
}
