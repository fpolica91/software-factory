#!/usr/bin/env node

import { createInterface } from 'node:readline/promises';
import type { Interface as ReadlineInterface } from 'node:readline/promises';
import { stdin, stdout } from 'node:process';
import type { JsonValue } from '@software-factory/harness-client';
import { CoordinatorClient, CoordinatorHttpError } from './coordinator-client.js';
import type {
  AttemptRecord,
  DurableJob,
  JobDefinition,
  JobState,
  OperationRecord,
  PendingRequestRecord,
  StageCheckpointRecord,
} from './types.js';

const POLL_INTERVAL_MS = 1_000;
const TERMINAL_JOB_STATES = new Set<JobState>([
  'succeeded',
  'failed',
  'cancelled',
]);

class UsageError extends Error {}
class DetachRequested extends Error {}
class JobStopped extends Error {}

interface RunOptions {
  task: string;
  detach: boolean;
  json: boolean;
  repository?: string;
  baseRef?: string;
  cwd?: string;
}

interface OutputOptions {
  json: boolean;
}

interface AttachResult {
  state?: JobState;
  needsInput?: boolean;
  detached?: boolean;
  interrupted?: boolean;
}

function usage(): string {
  return [
    'Software Factory',
    '',
    'Usage:',
    '  factory run [--detach] [--repo <git-url>] [--base-ref <ref>] [--cwd <path>] <task>',
    '  factory attach [--json] <job-id>',
    '  factory status [--json] <job-id>',
    '  factory stop [--json] <job-id>',
    '',
    'Examples:',
    '  factory run "Implement authentication"',
    '  factory run --detach "Review this codebase"',
    '  factory attach 7e455a37-...',
  ].join('\n');
}

function requiredValue(args: string[], index: number, flag: string): string {
  const value = args[index + 1];
  if (!value || value.startsWith('--')) {
    throw new UsageError(`${flag} requires a value`);
  }
  return value;
}

function parseRunOptions(args: string[]): RunOptions {
  let detach = false;
  let json = false;
  let repository: string | undefined;
  let baseRef: string | undefined;
  let cwd: string | undefined;
  const taskParts: string[] = [];

  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === '--detach') {
      detach = true;
      continue;
    }
    if (argument === '--json') {
      json = true;
      continue;
    }
    if (argument === '--repo') {
      repository = requiredValue(args, index, argument);
      index += 1;
      continue;
    }
    if (argument === '--base-ref') {
      baseRef = requiredValue(args, index, argument);
      index += 1;
      continue;
    }
    if (argument === '--cwd') {
      cwd = requiredValue(args, index, argument);
      index += 1;
      continue;
    }
    if (argument === '--') {
      taskParts.push(...args.slice(index + 1));
      break;
    }
    if (argument?.startsWith('--')) {
      throw new UsageError(`unknown run option ${argument}`);
    }
    if (argument) taskParts.push(argument);
  }

  const task = taskParts.join(' ').trim();
  if (!task) throw new UsageError('run requires a task');
  if (repository && cwd) {
    throw new UsageError('--repo and --cwd cannot be used together');
  }
  if (baseRef && !repository) {
    throw new UsageError('--base-ref requires --repo');
  }

  return {
    task,
    detach,
    json,
    ...(repository ? { repository } : {}),
    ...(baseRef ? { baseRef } : {}),
    ...(cwd ? { cwd } : {}),
  };
}

function parseJobOptions(args: string[]): { jobId: string; options: OutputOptions } {
  let json = false;
  const positional: string[] = [];
  for (const argument of args) {
    if (argument === '--json') json = true;
    else if (argument.startsWith('--')) throw new UsageError(`unknown option ${argument}`);
    else positional.push(argument);
  }
  if (positional.length !== 1 || !positional[0]?.trim()) {
    throw new UsageError('exactly one job id is required');
  }
  return { jobId: positional[0], options: { json } };
}

function taskPrompt(stage: 'plan' | 'execute' | 'review' | 'remediate', task: string): string {
  const original = `Original task:\n${task}`;
  switch (stage) {
    case 'plan':
      return [
        'Plan how to complete the task in this repository.',
        'Inspect the relevant code and record a concrete, actionable decomposition.',
        original,
      ].join('\n\n');
    case 'execute':
      return [
        'Carry out the original task using the durable plan.',
        'Verify the work and complete every current work unit.',
        original,
      ].join('\n\n');
    case 'review':
      return [
        'Review the completed work against the original task.',
        'Inspect the implementation and verification evidence, then record a structured verdict and findings.',
        original,
      ].join('\n\n');
    case 'remediate':
      return [
        'Address every current review finding, re-verify the work, and record each disposition.',
        'If the review approved the work, confirm that no remediation is needed.',
        original,
      ].join('\n\n');
  }
}

function jobDefinition(options: RunOptions): JobDefinition {
  const location: Record<string, JsonValue> = options.repository
    ? {
        workspace: {
          repository: options.repository,
          ...(options.baseRef ? { baseRef: options.baseRef } : {}),
        },
      }
    : { cwd: options.cwd ?? process.cwd() };
  return {
    kind: 'factory.task',
    input: {
      ...location,
      task: options.task,
      ...(process.env.FACTORY_PROJECT_ID
        ? { projectId: process.env.FACTORY_PROJECT_ID }
        : {}),
      approvalPolicy: 'on-request',
      sandbox: 'workspace-write',
    },
    operations: [
      {
        kind: 'codex.plan',
        input: { prompt: taskPrompt('plan', options.task) },
        maxAttempts: 3,
      },
      {
        kind: 'codex.execute',
        input: { prompt: taskPrompt('execute', options.task) },
        maxAttempts: 3,
      },
      {
        kind: 'codex.review',
        input: { prompt: taskPrompt('review', options.task) },
        maxAttempts: 3,
      },
      {
        kind: 'codex.remediate',
        input: { prompt: taskPrompt('remediate', options.task) },
        maxAttempts: 3,
      },
    ],
  };
}

function terminal(state: JobState): boolean {
  return TERMINAL_JOB_STATES.has(state);
}

function directProjectId(job: DurableJob): string | undefined {
  const input = record(job.job.input);
  if (typeof input?.projectId === 'string') return input.projectId;
  return input?.cwd === '/workspace/project' ? '' : undefined;
}

async function guardProjectMount(projectId: string): Promise<void> {
  const active = await new CoordinatorClient().listActiveJobs();
  const incompatible = active.find((job) => {
    const activeProjectId = directProjectId(job);
    return activeProjectId !== undefined && activeProjectId !== projectId;
  });
  if (!incompatible) return;
  const activeProjectId = directProjectId(incompatible);
  throw new Error(
    `job ${incompatible.job.jobId} is still active for ` +
    `${activeProjectId || 'an older unlabelled checkout'}; ` +
    `reattach or stop it before mounting ${projectId}`,
  );
}

function operationLabel(operation: OperationRecord): string {
  return operation.kind.startsWith('codex.')
    ? operation.kind.slice('codex.'.length)
    : operation.kind;
}

function jobSnapshot(job: DurableJob): string {
  return JSON.stringify({
    state: job.job.state,
    operations: job.operations.map((operation) => ({
      operationId: operation.operationId,
      state: operation.state,
      nextEligibleAt: operation.nextEligibleAt,
    })),
  });
}

function printProgress(job: DurableJob): void {
  console.log(`\nJob ${job.job.jobId} · ${job.job.state}`);
  for (const operation of [...job.operations].sort((left, right) => left.ordinal - right.ordinal)) {
    const retry = operation.state === 'retryWait'
      ? ` until ${operation.nextEligibleAt}`
      : '';
    console.log(`  ${operationLabel(operation).padEnd(10)} ${operation.state}${retry}`);
  }
}

function record(value: JsonValue | undefined): Record<string, JsonValue> | undefined {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, JsonValue>
    : undefined;
}

function text(value: JsonValue | undefined): string | undefined {
  return typeof value === 'string' && value.trim() ? value : undefined;
}

function stageOutput(stage: StageCheckpointRecord): string | undefined {
  const payload = record(stage.checkpoint.payload);
  const turn = record(payload?.turn);
  const items = Array.isArray(turn?.items) ? turn.items : [];
  const messages: string[] = [];
  const plans: string[] = [];
  for (const value of items) {
    const item = record(value);
    if (item?.type === 'agentMessage') {
      const message = text(item.text);
      if (message) messages.push(message);
    } else if (item?.type === 'plan') {
      const plan = text(item.text);
      if (plan) plans.push(plan);
    }
  }
  return messages.at(-1) ?? plans.at(-1);
}

function printStageOutputs(stages: StageCheckpointRecord[]): void {
  const ordered = [...stages].sort((left, right) => left.ordinal - right.ordinal);
  if (ordered.length === 0) return;
  console.log('\nResults');
  for (const stage of ordered) {
    const output = stageOutput(stage);
    if (!output) continue;
    const label = stage.operationKind.startsWith('codex.')
      ? stage.operationKind.slice('codex.'.length)
      : stage.operationKind;
    console.log(`\n${label[0]?.toUpperCase() ?? ''}${label.slice(1)}\n${output}`);
  }
}

function printAttemptFailures(attempts: AttemptRecord[]): void {
  const latest = new Map<string, AttemptRecord>();
  for (const attempt of attempts) latest.set(attempt.operationId, attempt);
  const failures = [...latest.values()].filter((attempt) => attempt.failure !== null);
  if (failures.length === 0) return;
  console.log('\nFailure details');
  for (const attempt of failures) {
    const failure = record(attempt.failure ?? undefined);
    const message = text(failure?.message) ?? displayValue(attempt.failure ?? undefined);
    console.log(`  Attempt ${attempt.attemptNumber}: ${message}`);
  }
}

function jsonObject(value: JsonValue, label: string): Record<string, JsonValue> {
  const result = record(value);
  if (!result) throw new Error(`${label} is not an object`);
  return result;
}

function displayValue(value: JsonValue | undefined): string {
  if (typeof value === 'string') return value;
  return JSON.stringify(value ?? null, null, 2);
}

async function askLine(prompt: string, hidden = false): Promise<string> {
  if (!stdin.isTTY || !stdout.isTTY) throw new DetachRequested();
  if (!hidden) {
    const interface_ = createInterface({ input: stdin, output: stdout });
    try {
      return await askWithDetach(interface_, prompt);
    } finally {
      interface_.close();
    }
  }
  return askHidden(prompt);
}

function askWithDetach(interface_: ReadlineInterface, prompt: string): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    let settled = false;
    const finish = (action: () => void) => {
      if (settled) return;
      settled = true;
      action();
    };
    interface_.once('SIGINT', () => finish(() => reject(new DetachRequested())));
    void interface_.question(prompt).then(
      (answer) => finish(() => resolve(answer)),
      (error: unknown) => finish(() => reject(error)),
    );
  });
}

function askHidden(prompt: string): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    const input = stdin;
    let answer = '';
    const wasRaw = input.isRaw;
    const cleanup = () => {
      input.off('data', onData);
      input.setRawMode(Boolean(wasRaw));
      input.pause();
    };
    const onData = (chunk: Buffer | string) => {
      const value = chunk.toString();
      for (const character of value) {
        if (character === '\u0003') {
          stdout.write('\n');
          cleanup();
          reject(new DetachRequested());
          return;
        }
        if (character === '\r' || character === '\n') {
          stdout.write('\n');
          cleanup();
          resolve(answer);
          return;
        }
        if (character === '\u007f' || character === '\b') {
          answer = answer.slice(0, -1);
          continue;
        }
        answer += character;
      }
    };
    stdout.write(prompt);
    input.resume();
    input.setRawMode(true);
    input.on('data', onData);
  });
}

async function choose<T extends string>(
  message: string,
  choices: Array<{ key: string; label: string; value: T }>,
): Promise<T> {
  if (choices.length === 0) throw new Error('no supported choices are available');
  console.log(message);
  for (const choice of choices) console.log(`  [${choice.key}] ${choice.label}`);
  while (true) {
    const answer = (await askLine('Choose: ')).trim().toLowerCase();
    const selected = choices.find((choice) =>
      choice.key.toLowerCase() === answer || choice.value.toLowerCase() === answer
    );
    if (selected) return selected.value;
    console.log('Enter one of the displayed choices.');
  }
}

function simpleApprovalChoices(
  params: Record<string, JsonValue>,
  includeSession: boolean,
): Array<{
  key: string;
  label: string;
  value: 'accept' | 'acceptForSession' | 'decline' | 'cancel';
}> {
  const available = Array.isArray(params.availableDecisions)
    ? new Set(params.availableDecisions.filter((value): value is string => typeof value === 'string'))
    : undefined;
  const candidates = [
    { key: 'a', label: 'accept once', value: 'accept' as const },
    ...(includeSession
      ? [{ key: 's', label: 'accept for this session', value: 'acceptForSession' as const }]
      : []),
    { key: 'd', label: 'decline', value: 'decline' as const },
    { key: 'c', label: 'cancel this action', value: 'cancel' as const },
  ];
  return available ? candidates.filter((choice) => available.has(choice.value)) : candidates;
}

async function commandApproval(params: Record<string, JsonValue>): Promise<JsonValue> {
  const command = displayValue(params.command ?? params.commandActions);
  const cwd = text(params.cwd);
  const reason = text(params.reason);
  const decision = await choose(
    [
      '\nCommand approval requested',
      `Command: ${command}`,
      ...(cwd ? [`Directory: ${cwd}`] : []),
      ...(reason ? [`Reason: ${reason}`] : []),
    ].join('\n'),
    simpleApprovalChoices(params, true),
  );
  return { decision };
}

async function fileApproval(params: Record<string, JsonValue>): Promise<JsonValue> {
  const reason = text(params.reason);
  const grantRoot = text(params.grantRoot);
  const decision = await choose(
    [
      '\nFile change approval requested',
      ...(grantRoot ? [`Root: ${grantRoot}`] : []),
      ...(reason ? [`Reason: ${reason}`] : []),
    ].join('\n'),
    simpleApprovalChoices(params, true),
  );
  return { decision };
}

function optionAnswers(raw: string, options: JsonValue[], allowOther: boolean): string[] | undefined {
  const tokens = raw.split(',').map((value) => value.trim()).filter(Boolean);
  if (tokens.length === 0) return [];
  const values: string[] = [];
  for (const token of tokens) {
    const number = Number(token);
    if (Number.isSafeInteger(number) && number >= 1 && number <= options.length) {
      const option = record(options[number - 1]);
      const label = text(option?.label);
      if (label) values.push(label);
      continue;
    }
    const matching = options
      .map((value) => text(record(value)?.label))
      .find((label) => label?.toLowerCase() === token.toLowerCase());
    if (matching) {
      values.push(matching);
      continue;
    }
    if (allowOther) {
      values.push(token);
      continue;
    }
    return undefined;
  }
  return values;
}

async function userInputResponse(params: Record<string, JsonValue>): Promise<JsonValue> {
  const questions = Array.isArray(params.questions) ? params.questions : [];
  const answers: Record<string, JsonValue> = {};
  for (const [index, value] of questions.entries()) {
    const question = jsonObject(value, `question ${index + 1}`);
    const id = text(question.id);
    if (!id) throw new Error(`question ${index + 1} has no id`);
    const header = text(question.header) ?? `Question ${index + 1}`;
    const prompt = text(question.question) ?? header;
    const options = Array.isArray(question.options) ? question.options : [];
    const allowOther = question.isOther === true;
    console.log(`\n${header}\n${prompt}`);
    for (const [optionIndex, optionValue] of options.entries()) {
      const option = jsonObject(optionValue, `option ${optionIndex + 1}`);
      console.log(
        `  ${optionIndex + 1}. ${text(option.label) ?? ''}` +
        `${text(option.description) ? ` — ${text(option.description)}` : ''}`,
      );
    }
    if (options.length > 0 && allowOther) console.log('  Or enter another answer.');
    while (true) {
      const raw = await askLine('Answer: ', question.isSecret === true);
      const selected = options.length > 0
        ? optionAnswers(raw, options, allowOther)
        : (raw.trim() ? [raw.trim()] : []);
      if (selected) {
        answers[id] = { answers: selected };
        break;
      }
      console.log('Choose an option number or label.');
    }
  }
  return { answers };
}

async function mcpElicitationResponse(params: Record<string, JsonValue>): Promise<JsonValue> {
  const mode = text(params.mode);
  console.log('\nMCP server input requested');
  console.log(`Server: ${text(params.serverName) ?? 'unknown'}`);
  console.log(`Message: ${text(params.message) ?? ''}`);
  if (mode === 'url') console.log(`URL: ${text(params.url) ?? ''}`);
  if (mode !== 'url') console.log(`Requested schema:\n${displayValue(params.requestedSchema)}`);
  const action = await choose('Response', [
    { key: 'a', label: 'accept', value: 'accept' as const },
    { key: 'd', label: 'decline', value: 'decline' as const },
    { key: 'c', label: 'cancel', value: 'cancel' as const },
  ]);
  if (action !== 'accept' || mode === 'url') {
    return { action, content: null, _meta: null };
  }
  while (true) {
    const raw = await askLine('JSON response (blank for null): ');
    if (!raw.trim()) return { action, content: null, _meta: null };
    try {
      return { action, content: JSON.parse(raw) as JsonValue, _meta: null };
    } catch {
      console.log('Enter valid JSON.');
    }
  }
}

async function permissionsResponse(
  coordinator: CoordinatorClient,
  jobId: string,
  params: Record<string, JsonValue>,
): Promise<JsonValue> {
  console.log('\nAdditional permissions requested');
  if (text(params.reason)) console.log(`Reason: ${text(params.reason)}`);
  if (text(params.cwd)) console.log(`Directory: ${text(params.cwd)}`);
  console.log(displayValue(params.permissions));
  const choice = await choose('The protocol supports a grant response; stop the job to refuse.', [
    { key: 't', label: 'grant for this turn', value: 'turn' as const },
    { key: 's', label: 'grant for this session', value: 'session' as const },
    { key: 'x', label: 'stop this job', value: 'stop' as const },
  ]);
  if (choice === 'stop') {
    await coordinator.cancelJob(jobId);
    throw new JobStopped();
  }
  return { permissions: params.permissions ?? {}, scope: choice };
}

async function pendingResponse(
  coordinator: CoordinatorClient,
  jobId: string,
  pending: PendingRequestRecord,
): Promise<JsonValue> {
  const params = jsonObject(pending.request.params, `${pending.request.method} params`);
  switch (pending.request.method) {
    case 'item/commandExecution/requestApproval':
      return commandApproval(params);
    case 'item/fileChange/requestApproval':
      return fileApproval(params);
    case 'item/tool/requestUserInput':
      return userInputResponse(params);
    case 'mcpServer/elicitation/request':
      return mcpElicitationResponse(params);
    case 'item/permissions/requestApproval':
      return permissionsResponse(coordinator, jobId, params);
    default:
      throw new Error(`unsupported durable request method ${pending.request.method}`);
  }
}

async function resolvePending(
  coordinator: CoordinatorClient,
  jobId: string,
  pending: PendingRequestRecord,
): Promise<void> {
  const response = await pendingResponse(coordinator, jobId, pending);
  try {
    await coordinator.resolvePendingRequest(pending.pendingRequestId, {
      response: {
        id: pending.request.id,
        method: pending.request.method,
        response,
      },
    });
    console.log(`Resolved ${pending.request.method}.`);
  } catch (error) {
    if (error instanceof CoordinatorHttpError && error.status === 409) {
      console.log('That request was already resolved or became inactive; refreshing.');
      return;
    }
    throw error;
  }
}

function pendingSummary(pending: PendingRequestRecord[]): Array<Record<string, JsonValue>> {
  return pending
    .filter((record_) => record_.state === 'pending')
    .map((record_) => ({
      pendingRequestId: record_.pendingRequestId,
      method: record_.request.method,
      createdAt: record_.createdAt,
    }));
}

async function delay(ms: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return;
  await new Promise<void>((resolve) => {
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      resolve();
    };
    signal.addEventListener('abort', onAbort, { once: true });
  });
}

async function attachJob(
  coordinator: CoordinatorClient,
  jobId: string,
  options: OutputOptions,
): Promise<AttachResult> {
  const controller = new AbortController();
  let requestedDetach = false;
  const detach = () => {
    requestedDetach = true;
    controller.abort();
  };
  process.once('SIGINT', detach);
  const prompted = new Set<string>();
  let previousSnapshot: string | undefined;
  try {
    while (!controller.signal.aborted) {
      const [job, requests] = await Promise.all([
        coordinator.loadJob(jobId),
        coordinator.listPendingRequests(jobId),
      ]);
      const snapshot = jobSnapshot(job);
      if (!options.json && snapshot !== previousSnapshot) printProgress(job);
      previousSnapshot = snapshot;

      const actionable = requests.filter((request) => request.state === 'pending');
      if (actionable.length > 0 && (!stdin.isTTY || !stdout.isTTY)) {
        if (options.json) {
          console.log(JSON.stringify({
            jobId,
            state: job.job.state,
            needsInput: pendingSummary(actionable),
          }));
        } else {
          console.log(`Job ${jobId} needs input. Run: factory attach ${jobId}`);
        }
        return { state: job.job.state, needsInput: true };
      }

      for (const pending of actionable) {
        if (prompted.has(pending.pendingRequestId)) continue;
        prompted.add(pending.pendingRequestId);
        try {
          await resolvePending(coordinator, jobId, pending);
        } catch (error) {
          if (error instanceof JobStopped) break;
          if (error instanceof DetachRequested) {
            detach();
            break;
          }
          throw error;
        }
      }

      if (terminal(job.job.state)) {
        const [stages, attempts] = await Promise.all([
          coordinator.listStageCheckpoints(jobId),
          coordinator.listJobAttempts(jobId),
        ]);
        if (options.json) {
          console.log(JSON.stringify({
            job,
            pendingRequests: pendingSummary(requests),
            stageCheckpoints: stages,
            attempts,
          }));
        } else {
          printStageOutputs(stages);
          printAttemptFailures(attempts);
        }
        return { state: job.job.state };
      }
      await delay(POLL_INTERVAL_MS, controller.signal);
    }
  } finally {
    process.off('SIGINT', detach);
  }

  if (requestedDetach) {
    if (options.json) console.log(JSON.stringify({ jobId, detached: true }));
    else {
      console.log(`\nDetached. Job ${jobId} is still running.`);
      console.log(`Reattach with: factory attach ${jobId}`);
    }
    return { detached: true, interrupted: true };
  }
  return {};
}

async function runCommand(options: RunOptions): Promise<AttachResult> {
  const coordinator = new CoordinatorClient();
  if (!options.repository) {
    const active = (await coordinator.listActiveJobs())
      .find((job) => directProjectId(job) !== undefined);
    if (active) {
      throw new Error(
        `job ${active.job.jobId} is already active in a local checkout; ` +
        `attach to it or stop it before starting another local job`,
      );
    }
  }
  const created = await coordinator.createJob(jobDefinition(options));
  const jobId = created.job.jobId;
  let workflowRunId: string;
  try {
    const { factoryJob } = await import('./hatchet.js');
    const run = await factoryJob.runNoWait({ jobId });
    workflowRunId = await run.getWorkflowRunId();
  } catch (error) {
    await coordinator.cancelJob(jobId).catch(() => undefined);
    throw new Error(
      `Hatchet dispatch failed; durable job ${jobId} was cancelled: ${String(error)}`,
    );
  }

  const shouldDetach = options.detach || options.json || !stdin.isTTY || !stdout.isTTY;
  if (shouldDetach) {
    if (options.json || !stdout.isTTY) {
      console.log(JSON.stringify({ jobId, workflowRunId, state: created.job.state }));
    } else {
      console.log(`Started job ${jobId}.`);
      console.log(`Attach with: factory attach ${jobId}`);
    }
    return { state: created.job.state, detached: true };
  }

  console.log(`Started job ${jobId} (Hatchet ${workflowRunId}).`);
  console.log('Ctrl-C detaches; it does not stop the job.');
  return attachJob(coordinator, jobId, { json: options.json });
}

async function statusCommand(jobId: string, options: OutputOptions): Promise<AttachResult> {
  const coordinator = new CoordinatorClient();
  const [job, requests, stages, attempts] = await Promise.all([
    coordinator.loadJob(jobId),
    coordinator.listPendingRequests(jobId),
    coordinator.listStageCheckpoints(jobId),
    coordinator.listJobAttempts(jobId),
  ]);
  if (options.json || !stdout.isTTY) {
    console.log(JSON.stringify({
      job,
      pendingRequests: pendingSummary(requests),
      stageCheckpoints: stages,
      attempts,
    }));
  } else {
    printProgress(job);
    const actionable = pendingSummary(requests);
    if (actionable.length > 0) {
      console.log(`\nNeeds input: ${actionable.length}. Run: factory attach ${jobId}`);
    }
    if (terminal(job.job.state)) {
      printStageOutputs(stages);
      printAttemptFailures(attempts);
    }
  }
  return { state: job.job.state, needsInput: pendingSummary(requests).length > 0 };
}

async function stopCommand(jobId: string, options: OutputOptions): Promise<AttachResult> {
  const coordinator = new CoordinatorClient();
  const job = await coordinator.cancelJob(jobId);
  if (options.json || !stdout.isTTY) console.log(JSON.stringify({ job }));
  else {
    console.log(`Stopped job ${jobId}.`);
    printProgress(job);
  }
  // A cancelled state is the successful outcome of `factory stop`, so the
  // command itself must still exit successfully.
  return {};
}

function resultExitCode(result: AttachResult): number {
  if (result.interrupted) return 130;
  if (result.needsInput) return 3;
  if (result.state === 'failed') return 1;
  if (result.state === 'cancelled') return 2;
  return 0;
}

async function main(): Promise<void> {
  const [command, ...args] = process.argv.slice(2);
  if (!command || command === 'help' || command === '--help' || command === '-h') {
    console.log(usage());
    return;
  }

  let result: AttachResult;
  switch (command) {
    case 'run':
      result = await runCommand(parseRunOptions(args));
      break;
    case 'attach': {
      const { jobId, options } = parseJobOptions(args);
      result = await attachJob(new CoordinatorClient(), jobId, options);
      break;
    }
    case 'status': {
      const { jobId, options } = parseJobOptions(args);
      result = await statusCommand(jobId, options);
      break;
    }
    case 'stop': {
      const { jobId, options } = parseJobOptions(args);
      result = await stopCommand(jobId, options);
      break;
    }
    case 'guard-project': {
      if (args.length !== 1 || !args[0]?.trim()) {
        throw new UsageError('guard-project requires one canonical project path');
      }
      await guardProjectMount(args[0]);
      result = {};
      break;
    }
    default:
      throw new UsageError(`unknown command ${command}`);
  }
  process.exitCode = resultExitCode(result);
}

main().catch((error: unknown) => {
  if (error instanceof UsageError) {
    console.error(`${error.message}\n\n${usage()}`);
    process.exitCode = 2;
    return;
  }
  if (error instanceof CoordinatorHttpError) {
    console.error(error.message);
    process.exitCode = 1;
    return;
  }
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});
