import {
  FactoryClient,
  type FactoryCorrelatedCodexNotification,
  type FactoryCorrelation,
  type FactoryCorrelationSeed,
  type FactoryRawServerRequest,
  type JsonValue,
} from '@software-factory/harness-client';
import type {
  CodexServerRequestEvent,
  CodexServerRequestResponse,
  CodexExperimentalTurnStartParams,
} from '@software-factory/harness-client/codex-v2';
import type {
  ErrorNotification,
  ReviewStartResponse,
  ThreadResumeParams,
  ThreadStartParams,
  Turn,
  TurnCompletedNotification,
  TurnInterruptResponse,
  TurnStartResponse,
} from '@software-factory/harness-client/codex-v2/v2';
import { CoordinatorClient, CoordinatorHttpError } from './coordinator-client.js';
import type {
  CheckpointRecord,
  CodexOperationInput,
  OperationKind,
  OperationResult,
  RecoveryLease,
} from './types.js';

export class TerminalOperationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'TerminalOperationError';
  }
}

export class JobCancelledError extends TerminalOperationError {
  constructor() {
    super('Factory job was cancelled');
    this.name = 'JobCancelledError';
  }
}

const HUMAN_SERVER_REQUEST_METHODS = new Set([
  'item/commandExecution/requestApproval',
  'item/fileChange/requestApproval',
  'item/tool/requestUserInput',
  'mcpServer/elicitation/request',
  'item/permissions/requestApproval',
]);

function abortFailure(): Error {
  return new Error('durable Codex server-request wait was cancelled');
}

function delay(ms: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.reject(abortFailure());
  return new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(abortFailure());
    };
    signal.addEventListener('abort', onAbort, { once: true });
  });
}

function isTransientCoordinatorError(error: unknown): boolean {
  return error instanceof CoordinatorHttpError ? error.status >= 500 : error instanceof TypeError;
}

function userInputAutoResolutionMs(request: FactoryRawServerRequest): number | undefined {
  if (request.method !== 'item/tool/requestUserInput') return undefined;
  if (typeof request.params !== 'object' || request.params === null || Array.isArray(request.params)) {
    return undefined;
  }
  const value = (request.params as Record<string, JsonValue>).autoResolutionMs;
  return typeof value === 'number' && Number.isFinite(value) && value >= 0
    ? value
    : undefined;
}

function autonomousServerRequestResponse(
  request: FactoryRawServerRequest,
): JsonValue | undefined {
  switch (request.method) {
    case 'item/commandExecution/requestApproval':
    case 'item/fileChange/requestApproval':
      return { decision: 'accept' };
    case 'item/tool/requestUserInput':
      return { answers: {} };
    case 'mcpServer/elicitation/request':
      return { action: 'decline', content: null, _meta: null };
    case 'item/permissions/requestApproval': {
      const params = typeof request.params === 'object' && request.params !== null &&
        !Array.isArray(request.params)
        ? request.params as Record<string, JsonValue>
        : {};
      return { permissions: params.permissions ?? {}, scope: 'session' };
    }
    default:
      return undefined;
  }
}

async function waitForJobCancellation(
  isCancelled: () => Promise<boolean>,
  signal: AbortSignal,
): Promise<never> {
  while (true) {
    if (signal.aborted) throw abortFailure();
    try {
      if (await isCancelled()) throw new JobCancelledError();
    } catch (error) {
      if (error instanceof JobCancelledError) throw error;
      if (!isTransientCoordinatorError(error)) throw error;
    }
    await delay(750, signal);
  }
}

async function withCoordinatorRetry<T>(
  action: () => Promise<T>,
  signal: AbortSignal,
): Promise<T> {
  while (true) {
    if (signal.aborted) throw abortFailure();
    try {
      return await action();
    } catch (error) {
      if (signal.aborted || !isTransientCoordinatorError(error)) throw error;
      await delay(500, signal);
    }
  }
}

function durableCodexServerRequestHandler(options: {
  coordinator: CoordinatorClient;
  attemptId: string;
  autonomous: boolean;
  signal: AbortSignal;
  log(message: string): Promise<void>;
}): (
  event: Extract<CodexServerRequestEvent, { kind: 'known' }>,
) => Promise<CodexServerRequestResponse> | undefined {
  const { coordinator, attemptId, autonomous, signal, log } = options;
  return (event) => {
    const { request } = event;
    if (!HUMAN_SERVER_REQUEST_METHODS.has(request.method)) return undefined;
    return (async () => {
      const rawRequest: FactoryRawServerRequest = {
        id: request.id,
        method: request.method,
        params: request.params as unknown as JsonValue,
      };
      const pending = await withCoordinatorRetry(
        () => coordinator.registerPendingRequest({ attemptId, request: rawRequest }),
        signal,
      );
      const autonomousResponse = autonomous
        ? autonomousServerRequestResponse(rawRequest)
        : undefined;
      if (autonomousResponse !== undefined) {
        let response = autonomousResponse;
        try {
          await withCoordinatorRetry(
            () => coordinator.resolvePendingRequest(pending.pendingRequestId, {
              response: {
                id: rawRequest.id,
                method: rawRequest.method,
                response: autonomousResponse,
              },
            }),
            signal,
          );
        } catch (error) {
          if (!(error instanceof CoordinatorHttpError) || error.status !== 409) throw error;
          const current = await withCoordinatorRetry(
            () => coordinator.loadPendingRequest(pending.pendingRequestId),
            signal,
          );
          if (current.state !== 'resolved' || !current.response ||
            current.response.id !== rawRequest.id ||
            current.response.method !== rawRequest.method) {
            throw new TerminalOperationError(
              `pending request ${current.pendingRequestId} conflicted without a related resolution`,
            );
          }
          response = current.response.response;
        }
        await log(`auto-resolved ${request.method} as ${pending.pendingRequestId}`);
        return { result: response };
      }
      const autoResolutionMs = userInputAutoResolutionMs(rawRequest);
      const autoResolutionAt = autoResolutionMs === undefined
        ? undefined
        : new Date(pending.createdAt).getTime() + autoResolutionMs;
      await log(
        `waiting for ${request.method} resolution as ${pending.pendingRequestId}`,
      );
      while (true) {
        const current = await withCoordinatorRetry(
          () => coordinator.loadPendingRequest(pending.pendingRequestId),
          signal,
        );
        if (current.state === 'inactive') {
          throw new TerminalOperationError(
            `pending request ${current.pendingRequestId} lost its active attempt lease`,
          );
        }
        if (current.state === 'pending' && autoResolutionAt !== undefined &&
          Date.now() >= autoResolutionAt) {
          try {
            await coordinator.resolvePendingRequest(current.pendingRequestId, {
              response: {
                id: rawRequest.id,
                method: rawRequest.method,
                response: { answers: {} },
              },
            });
            await log(`auto-resolved ${request.method} as ${current.pendingRequestId}`);
          } catch (error) {
            if (!(error instanceof CoordinatorHttpError) || error.status !== 409) throw error;
          }
          continue;
        }
        if (current.state === 'resolved') {
          const response = current.response;
          if (!response || response.id !== rawRequest.id || response.method !== rawRequest.method) {
            throw new TerminalOperationError(
              `pending request ${current.pendingRequestId} returned an unrelated response`,
            );
          }
          await log(`resolved ${request.method} as ${current.pendingRequestId}`);
          return { result: response.response };
        }
        await delay(1_000, signal);
      }
    })();
  };
}

class CorrelationRecorder {
  readonly #records = new Map<string, string>();
  latestCorrelationId: string | undefined;

  constructor(readonly coordinator: CoordinatorClient) {}

  async persist(correlation: FactoryCorrelation): Promise<string> {
    const key = JSON.stringify(correlation);
    const existing = this.#records.get(key);
    if (existing) return existing;
    const record = await this.coordinator.appendCorrelation(correlation);
    this.#records.set(key, record.correlationId);
    this.latestCorrelationId = record.correlationId;
    return record.correlationId;
  }
}

interface TerminalTurn {
  turn: Turn;
  correlationId?: string;
}

class TurnMonitor {
  readonly #terminalTurns = new Map<string, TerminalTurn>();
  readonly #waiters = new Map<string, {
    resolve(value: TerminalTurn): void;
    reject(error: Error): void;
  }>();
  #failure: Error | undefined;

  constructor(readonly correlations: CorrelationRecorder) {}

  async observe(correlated: FactoryCorrelatedCodexNotification): Promise<void> {
    const correlationId = correlated.correlation
      ? await this.correlations.persist(correlated.correlation)
      : undefined;
    const { notification } = correlated.notification;
    if (notification.method === 'error') {
      const params = notification.params as ErrorNotification;
      if (!params.willRetry) {
        this.fail(new Error(`Codex runtime error: ${params.error.message}`));
      }
      return;
    }
    if (notification.method !== 'turn/completed') return;
    const params = notification.params as TurnCompletedNotification;
    const terminal = {
      turn: params.turn,
      ...(correlationId ? { correlationId } : {}),
    };
    this.#terminalTurns.set(params.turn.id, terminal);
    const waiter = this.#waiters.get(params.turn.id);
    if (waiter) {
      this.#waiters.delete(params.turn.id);
      waiter.resolve(terminal);
    }
  }

  fail(error: Error): void {
    if (this.#failure) return;
    this.#failure = error;
    for (const waiter of this.#waiters.values()) waiter.reject(error);
    this.#waiters.clear();
  }

  waitForTurn(
    turnId: string,
    timeoutMs: number,
    signal?: AbortSignal,
  ): Promise<TerminalTurn> {
    if (this.#failure) return Promise.reject(this.#failure);
    const completed = this.#terminalTurns.get(turnId);
    if (completed) return Promise.resolve(completed);
    if (signal?.aborted) return Promise.reject(new Error('Codex turn wait was cancelled'));
    return new Promise<TerminalTurn>((resolve, reject) => {
      const onAbort = () => {
        clearTimeout(timer);
        this.#waiters.delete(turnId);
        reject(new Error('Codex turn wait was cancelled'));
      };
      const timer = setTimeout(() => {
        signal?.removeEventListener('abort', onAbort);
        this.#waiters.delete(turnId);
        reject(new Error(`timed out waiting for Codex turn ${turnId}`));
      }, timeoutMs);
      signal?.addEventListener('abort', onAbort, { once: true });
      this.#waiters.set(turnId, {
        resolve: (value) => {
          clearTimeout(timer);
          signal?.removeEventListener('abort', onAbort);
          resolve(value);
        },
        reject: (error) => {
          clearTimeout(timer);
          signal?.removeEventListener('abort', onAbort);
          reject(error);
        },
      });
    });
  }
}

function checkpointFor(lease: RecoveryLease): CheckpointRecord | undefined {
  return lease.selection.resume.kind === 'fromCheckpoint'
    ? lease.selection.resume.checkpoint
    : undefined;
}

export function checkpointThreadId(lease: RecoveryLease): string | undefined {
  const checkpoint = checkpointFor(lease);
  if (!checkpoint || typeof checkpoint.payload !== 'object' || checkpoint.payload === null) {
    return undefined;
  }
  const value = (checkpoint.payload as Record<string, JsonValue>).threadId;
  return typeof value === 'string' ? value : undefined;
}

export function hasCompletedStageCheckpoint(lease: RecoveryLease, kind: OperationKind): boolean {
  const checkpoint = checkpointFor(lease);
  if (!checkpoint || checkpoint.kind !== `${kind}.completed`) return false;
  if (typeof checkpoint.payload !== 'object' || checkpoint.payload === null) return false;
  const payload = checkpoint.payload as Record<string, JsonValue>;
  if (payload.stage !== kind || payload.turnStatus !== 'completed') return false;
  return kind !== 'codex.remediate' || payload.reviewLoopComplete === true;
}

function threadStartRequest(input: CodexOperationInput): ThreadStartParams {
  return {
    cwd: input.cwd,
    ephemeral: false,
    ...(input.approvalPolicy ? { approvalPolicy: input.approvalPolicy } : {}),
    ...(input.sandbox ? { sandbox: input.sandbox } : {}),
    ...(input.model ? { model: input.model } : {}),
    ...(input.modelProvider ? { modelProvider: input.modelProvider } : {}),
    ...(input.config ? { config: input.config } : {}),
    ...(input.personality ? { personality: input.personality } : {}),
    ...(input.developerInstructions
      ? { developerInstructions: input.developerInstructions }
      : {}),
  };
}

function threadResumeRequest(threadId: string, input: CodexOperationInput): ThreadResumeParams {
  return {
    threadId,
    cwd: input.cwd,
    ...(input.approvalPolicy ? { approvalPolicy: input.approvalPolicy } : {}),
    ...(input.sandbox ? { sandbox: input.sandbox } : {}),
    ...(input.model ? { model: input.model } : {}),
    ...(input.modelProvider ? { modelProvider: input.modelProvider } : {}),
    ...(input.config ? { config: input.config } : {}),
    ...(input.personality ? { personality: input.personality } : {}),
    ...(input.developerInstructions
      ? { developerInstructions: input.developerInstructions }
      : {}),
  };
}

function processEnvironment(input: CodexOperationInput): NodeJS.ProcessEnv | undefined {
  if (!input.env && !input.codexHome) return undefined;
  return {
    ...input.env,
    ...(input.codexHome ? { CODEX_HOME: input.codexHome } : {}),
  };
}

export async function executeCodexOperation(options: {
  coordinator: CoordinatorClient;
  lease: RecoveryLease;
  kind: OperationKind;
  input: CodexOperationInput;
  correlation: FactoryCorrelationSeed;
  checkpointMetadata?: Record<string, JsonValue>;
  completionCheckpointKind?: string;
  expectedThreadId?: string;
  refreshWorkspaceRevision?(): Promise<string>;
  isCancelled?(): Promise<boolean>;
  log(message: string): Promise<void>;
}): Promise<OperationResult> {
  const {
    coordinator,
    lease,
    kind,
    input,
    correlation,
    checkpointMetadata,
    completionCheckpointKind,
    expectedThreadId,
    refreshWorkspaceRevision,
    isCancelled,
    log,
  } = options;
  const correlations = new CorrelationRecorder(coordinator);
  const monitor = new TurnMonitor(correlations);
  const pendingRequestAbort = new AbortController();
  let activeThreadId: string | undefined;
  let activeTurnId: string | undefined;
  let activeModel: string | undefined;
  const client = await FactoryClient.connect({
    runtimePath: input.runtimePath ?? process.env.FACTORY_RUNTIME_PATH ?? 'factory-runtime',
    ...(input.runtimeArgs ? { runtimeArgs: input.runtimeArgs } : {}),
    cwd: input.cwd,
    ...(processEnvironment(input) ? { env: processEnvironment(input) } : {}),
    onCodexCorrelatedNotification: (notification) => monitor.observe(notification),
    onCodexServerRequest: durableCodexServerRequestHandler({
      coordinator,
      attemptId: lease.attempt.attemptId,
      autonomous: input.approvalPolicy === 'never',
      signal: pendingRequestAbort.signal,
      log,
    }),
    onError: (error) => monitor.fail(error),
    onSignal: (signal) => {
      if (signal.type === 'terminateOperation') {
        monitor.fail(new TerminalOperationError(
          `runtime requested operation termination: ${signal.resolution.response.error.message}`,
        ));
      }
    },
  });

  try {
    const checkpointId = checkpointThreadId(lease);
    if (checkpointId) {
      const resumed = await client.requestCodexCorrelated(
        'thread/resume',
        threadResumeRequest(checkpointId, input),
        correlation,
      );
      const correlationId = await correlations.persist(resumed.correlation);
      activeThreadId = resumed.response.thread.id;
      activeModel = resumed.response.model;
      if (activeThreadId !== checkpointId) {
        throw new TerminalOperationError(
          `thread lineage changed while resuming ${checkpointId}: got ${activeThreadId}`,
        );
      }
      await log(`resumed Codex thread ${activeThreadId} for ${kind}`);
      await coordinator.saveCheckpoint({
        attemptId: lease.attempt.attemptId,
        kind: `${kind}.thread-bound`,
        payload: {
          ...checkpointMetadata,
          stage: kind,
          phase: 'threadBound',
          threadId: activeThreadId,
          resumed: true,
          requestId: resumed.requestId,
          requestMethod: resumed.method,
        },
        workspaceRoot: input.cwd,
        ...(input.workspaceRevision ? { workspaceRevision: input.workspaceRevision } : {}),
        correlationId,
      });
    } else {
      const started = await client.requestCodexCorrelated(
        'thread/start',
        threadStartRequest(input),
        correlation,
      );
      const correlationId = await correlations.persist(started.correlation);
      activeThreadId = started.response.thread.id;
      activeModel = started.response.model;
      await log(`started Codex thread ${activeThreadId} for ${kind}`);
      await coordinator.saveCheckpoint({
        attemptId: lease.attempt.attemptId,
        kind: `${kind}.thread-bound`,
        payload: {
          ...checkpointMetadata,
          stage: kind,
          phase: 'threadBound',
          threadId: activeThreadId,
          resumed: false,
          requestId: started.requestId,
          requestMethod: started.method,
        },
        workspaceRoot: input.cwd,
        ...(input.workspaceRevision ? { workspaceRevision: input.workspaceRevision } : {}),
        correlationId,
      });
    }

    if (expectedThreadId && activeThreadId !== expectedThreadId) {
      throw new TerminalOperationError(
        `stage ${kind} expected Codex thread ${expectedThreadId}, got ${activeThreadId}`,
      );
    }
    if (!activeThreadId) {
      throw new TerminalOperationError(`runtime did not bind a Codex thread for ${kind}`);
    }
    if (!activeModel) {
      throw new TerminalOperationError(`runtime did not report the active Codex model for ${kind}`);
    }
    const threadId = activeThreadId;

    const mode = kind === 'codex.plan'
      ? 'plan'
      : kind === 'codex.review'
        ? 'review'
        : 'normal';
    let startedOperation:
      | Awaited<ReturnType<typeof client.requestCodexCorrelated<'turn/start'>>>
      | Awaited<ReturnType<typeof client.requestCodexCorrelated<'review/start'>>>;
    if (kind === 'codex.review') {
      startedOperation = await client.requestCodexCorrelated('review/start', {
        threadId,
        delivery: 'inline',
        target: { type: 'custom', instructions: input.prompt },
      }, correlation);
      if (startedOperation.response.reviewThreadId !== threadId) {
        throw new TerminalOperationError(
          `inline review changed thread lineage from ${threadId} to ${startedOperation.response.reviewThreadId}`,
        );
      }
    } else {
      const turnParams: CodexExperimentalTurnStartParams = {
        threadId,
        input: [{ type: 'text', text: input.prompt, text_elements: [] }],
        cwd: input.cwd,
        clientUserMessageId: input.clientUserMessageId ?? `factory:${lease.selection.operationId}`,
        ...(input.model ? { model: input.model } : {}),
        ...(input.outputSchema ? { outputSchema: input.outputSchema } : {}),
        collaborationMode: {
          mode: kind === 'codex.plan' ? 'plan' : 'default',
          settings: {
            model: activeModel,
            reasoning_effort: null,
            developer_instructions: null,
          },
        },
      };
      startedOperation = await client.requestCodexCorrelated(
        'turn/start',
        turnParams,
        correlation,
      );
    }
    await correlations.persist(startedOperation.correlation);
    const turnId = startedOperation.response.turn.id;
    activeTurnId = turnId;
    await log(`started exact Codex V2 ${startedOperation.method} ${turnId} for ${kind}`);

    const cancellationAbort = isCancelled ? new AbortController() : undefined;
    const turnCompletion = monitor.waitForTurn(
      turnId,
      (input.turnTimeoutSeconds ?? 4 * 60 * 60) * 1000,
      cancellationAbort?.signal,
    );
    let terminal: TerminalTurn;
    if (isCancelled && cancellationAbort) {
      try {
        terminal = await Promise.race([
          turnCompletion,
          waitForJobCancellation(isCancelled, cancellationAbort.signal),
        ]);
      } finally {
        cancellationAbort.abort();
      }
    } else {
      terminal = await turnCompletion;
    }
    if (terminal.turn.status !== 'completed') {
      throw new Error(
        `Codex turn ${turnId} ended ${terminal.turn.status}: ${terminal.turn.error?.message ?? 'no detail'}`,
      );
    }

    const completedWorkspaceRevision = refreshWorkspaceRevision
      ? await refreshWorkspaceRevision()
      : input.workspaceRevision;
    const checkpoint = await coordinator.saveCheckpoint({
      attemptId: lease.attempt.attemptId,
      kind: completionCheckpointKind ?? `${kind}.completed`,
      payload: {
        ...checkpointMetadata,
        stage: kind,
        mode,
        phase: 'completed',
        threadId,
        turnId,
        requestId: startedOperation.requestId,
        requestMethod: startedOperation.method,
        turnStatus: terminal.turn.status,
        turn: terminal.turn as unknown as JsonValue,
      },
      workspaceRoot: input.cwd,
      ...(completedWorkspaceRevision
        ? { workspaceRevision: completedWorkspaceRevision }
        : {}),
      ...(terminal.correlationId ?? correlations.latestCorrelationId
        ? { correlationId: terminal.correlationId ?? correlations.latestCorrelationId }
        : {}),
    });
    await log(`checkpointed ${kind} turn ${turnId} as ${checkpoint.checkpointId}`);
    return {
      threadId,
      turnId,
      turn: terminal.turn,
      ...(completedWorkspaceRevision
        ? { workspaceRevision: completedWorkspaceRevision }
        : {}),
      recoveredFromCheckpoint: checkpointId !== undefined,
    };
  } catch (error) {
    pendingRequestAbort.abort();
    if (activeThreadId && activeTurnId) {
      await client.requestCodexCorrelated(
        'turn/interrupt',
        { threadId: activeThreadId, turnId: activeTurnId },
        correlation,
      ).then((result) => correlations.persist(result.correlation)).catch(() => undefined);
    }
    throw error;
  } finally {
    pendingRequestAbort.abort();
    await client.close().catch(() => undefined);
  }
}
