import type { JsonValue } from '@software-factory/harness-client';
import { CoordinatorClient, CoordinatorHttpError } from './coordinator-client.js';
import {
  FACTORY_STAGE_CONTRACT,
  FactoryStateGateError,
  assertFactoryStageState,
} from './factory-state-gate.js';
import { IntegrationLifecyclePublisher } from './integration-lifecycle.js';
import {
  JobCancelledError,
  TerminalOperationError,
  checkpointThreadId,
  executeCodexOperation,
  hasCompletedStageCheckpoint,
} from './lifecycle.js';
import {
  OPERATION_KINDS,
  type CodexOperationInput,
  type DurableJob,
  type FactoryJobContext,
  type FactoryJobInput,
  type FactoryJobResult,
  type FactoryThreadStateDocument,
  type FactoryWorkspaceRequest,
  type OperationKind,
  type OperationRecord,
  type OperationResult,
  type RecoveryLease,
  type WorkspaceRecord,
} from './types.js';

const KIND_ORDER = new Map<OperationKind, number>(OPERATION_KINDS.map((kind, index) => [kind, index]));
const DEFAULT_LEASE_SECONDS = 15 * 60;
const DEFAULT_CLAIM_POLL_SECONDS = 5;

async function enforceFactoryStageState(options: {
  coordinator: CoordinatorClient;
  lease: RecoveryLease;
  stage: OperationKind;
  threadId: string;
  workspaceRoot?: string;
  workspaceRevision?: string;
  log(message: string): Promise<void>;
}): Promise<void> {
  const {
    coordinator,
    lease,
    stage,
    threadId,
    workspaceRoot,
    workspaceRevision,
    log,
  } = options;
  let state: FactoryThreadStateDocument;
  try {
    state = (await coordinator.getThreadState(threadId)).state;
  } catch (error) {
    if (!(error instanceof CoordinatorHttpError) || error.status !== 404) throw error;
    state = {};
  }

  try {
    assertFactoryStageState(stage, state);
  } catch (error) {
    if (!(error instanceof FactoryStateGateError)) throw error;
    await coordinator.saveCheckpoint({
      attemptId: lease.attempt.attemptId,
      kind: `${stage}.semantic-gate-failed`,
      payload: {
        stage,
        phase: 'semanticGateFailed',
        threadId,
        message: error.message,
      },
      ...(workspaceRoot ? { workspaceRoot } : {}),
      ...(workspaceRevision ? { workspaceRevision } : {}),
    });
    await log(error.message);
    throw error;
  }
}

function object(value: JsonValue, label: string): Record<string, JsonValue> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new TerminalOperationError(`${label} must be a JSON object`);
  }
  return value as Record<string, JsonValue>;
}

function operationKind(value: string): OperationKind {
  if ((OPERATION_KINDS as readonly string[]).includes(value)) return value as OperationKind;
  throw new TerminalOperationError(`unsupported factory operation kind ${value}`);
}

function deploymentCodexDefaults(): Record<string, JsonValue> {
  const defaults: Record<string, JsonValue> = {};
  const environmentFields = {
    runtimePath: process.env.FACTORY_RUNTIME_PATH,
    codexHome: process.env.FACTORY_CODEX_HOME,
    model: process.env.FACTORY_MODEL,
    modelProvider: process.env.FACTORY_MODEL_PROVIDER,
  };
  for (const [field, value] of Object.entries(environmentFields)) {
    if (value) defaults[field] = value;
  }

  const config: Record<string, JsonValue> = {};
  const catalog = process.env.FACTORY_MODEL_CATALOG_JSON;
  if (catalog) config.model_catalog_json = catalog;
  const providerId = process.env.FACTORY_MODEL_PROVIDER;
  const providerBaseUrl = process.env.FACTORY_PROVIDER_BASE_URL;
  if (providerId && providerBaseUrl) {
    config[`model_providers.${providerId}`] = {
      name: process.env.FACTORY_PROVIDER_NAME ?? 'Software Factory deployment provider',
      base_url: providerBaseUrl,
      wire_api: 'responses',
      requires_openai_auth: false,
      supports_websockets: false,
      ...(providerId === 'factory-provider'
        ? {
            env_http_headers: {
              'X-OpenCodex-API-Key': 'FACTORY_PROVIDER_AUTH_TOKEN',
            },
          }
        : {}),
    };
  }
  if (Object.keys(config).length > 0) defaults.config = config;
  return defaults;
}

function workspaceRequest(
  jobInput: Record<string, JsonValue>,
  workflowRequest?: JsonValue,
): FactoryWorkspaceRequest | undefined {
  const value = workflowRequest ?? jobInput.workspace;
  if (value === undefined) return undefined;
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new TerminalOperationError('job input.workspace must be a JSON object');
  }
  const request = value as Record<string, JsonValue>;
  if (typeof request.repository !== 'string' || !request.repository.trim()) {
    throw new TerminalOperationError('job input.workspace.repository must be a non-empty string');
  }
  if (request.baseRef !== undefined &&
    (typeof request.baseRef !== 'string' || !request.baseRef.trim())) {
    throw new TerminalOperationError('job input.workspace.baseRef must be a non-empty string');
  }
  return {
    repository: request.repository,
    ...(typeof request.baseRef === 'string' ? { baseRef: request.baseRef } : {}),
  };
}

interface ResolvedCodexInput {
  input: CodexOperationInput;
  usesManagedWorkspace: boolean;
}

function codexInput(
  job: DurableJob,
  operation: OperationRecord,
  workspace?: WorkspaceRecord,
): ResolvedCodexInput {
  const defaults = deploymentCodexDefaults();
  const jobInput = object(job.job.input, 'job input');
  const stageInput = object(operation.input, `operation ${operation.operationId} input`);
  const { workspace: _workspace, ...sharedJobInput } = jobInput;
  const merged = {
    ...defaults,
    ...sharedJobInput,
    ...stageInput,
    ...(defaults.config || sharedJobInput.config || stageInput.config
      ? {
          config: {
            ...(object((defaults.config ?? {}) as JsonValue, 'deployment config')),
            ...(object((sharedJobInput.config ?? {}) as JsonValue, 'job config')),
            ...(object((stageInput.config ?? {}) as JsonValue, 'operation config')),
          },
        }
      : {}),
  } as Record<string, JsonValue>;
  const usesManagedWorkspace = workspace !== undefined &&
    !Object.prototype.hasOwnProperty.call(stageInput, 'cwd');
  if (usesManagedWorkspace) {
    merged.cwd = workspace.root;
    merged.workspaceRevision = workspace.revision;
  }
  if (typeof merged.cwd !== 'string' || !merged.cwd) {
    throw new TerminalOperationError(
      `operation ${operation.operationId} requires input.cwd or job input.workspace`,
    );
  }
  if (typeof merged.prompt !== 'string' || !merged.prompt) {
    throw new TerminalOperationError(`operation ${operation.operationId} requires input.prompt`);
  }
  const existingInstructions = merged.developerInstructions;
  if (existingInstructions !== undefined && typeof existingInstructions !== 'string') {
    throw new TerminalOperationError(
      `operation ${operation.operationId} input.developerInstructions must be a string`,
    );
  }
  merged.developerInstructions = existingInstructions?.trim()
    ? `${existingInstructions}\n\n${FACTORY_STAGE_CONTRACT[operationKind(operation.kind)]}`
    : FACTORY_STAGE_CONTRACT[operationKind(operation.kind)];
  return {
    input: merged as unknown as CodexOperationInput,
    usesManagedWorkspace,
  };
}

function failureDetail(error: unknown, operation: OperationRecord): JsonValue {
  const failure = error instanceof Error ? error : new Error(String(error));
  return {
    operationId: operation.operationId,
    operationKind: operation.kind,
    name: failure.name,
    message: failure.message,
  };
}

function validateOperationOrder(operations: OperationRecord[]): void {
  let previous = -1;
  for (const operation of operations) {
    const rank = KIND_ORDER.get(operationKind(operation.kind));
    if (rank === undefined || rank < previous) {
      throw new TerminalOperationError(
        `job operation order must follow ${OPERATION_KINDS.join(' -> ')}`,
      );
    }
    previous = rank;
  }
}

class LeaseRenewer {
  #timer: NodeJS.Timeout | undefined;
  #failure: Error | undefined;

  constructor(
    readonly coordinator: CoordinatorClient,
    readonly lease: RecoveryLease,
    readonly ownerInstanceId: string,
    readonly leaseSeconds: number,
  ) {}

  start(): void {
    const intervalMs = Math.max(10_000, Math.floor(this.leaseSeconds * 1000 / 3));
    this.#timer = setInterval(() => {
      void this.coordinator.renewAttempt(this.lease.attempt.attemptId, {
        ownerInstanceId: this.ownerInstanceId,
        leaseSeconds: this.leaseSeconds,
      }).catch((error: unknown) => {
        this.#failure = error instanceof Error ? error : new Error(String(error));
      });
    }, intervalMs);
  }

  stop(): void {
    if (this.#timer) clearInterval(this.#timer);
    this.#timer = undefined;
  }

  assertHealthy(): void {
    if (this.#failure) throw this.#failure;
  }
}

async function sleepFor(context: FactoryJobContext, seconds: number): Promise<void> {
  await context.sleepUntil(new Date(Date.now() + seconds * 1000));
}

function operationFrom(job: DurableJob, operationId: string): OperationRecord {
  const operation = job.operations.find((candidate) => candidate.operationId === operationId);
  if (!operation) throw new TerminalOperationError(`factoryd no longer returns operation ${operationId}`);
  return operation;
}

export async function executeFactoryJob(
  input: FactoryJobInput,
  context: FactoryJobContext,
  coordinator = new CoordinatorClient(),
): Promise<FactoryJobResult> {
  const initial = await coordinator.loadJob(input.jobId);
  if (initial.job.state === 'cancelled') throw new JobCancelledError();
  const ordered = [...initial.operations].sort((left, right) => left.ordinal - right.ordinal);
  validateOperationOrder(ordered);
  const jobInput = object(initial.job.input, 'job input');
  const integration = await IntegrationLifecyclePublisher.fromJobInput(jobInput);
  const requestedWorkspace = workspaceRequest(
    jobInput,
    input.workspace,
  );
  let managedWorkspace = requestedWorkspace
    ? await coordinator.ensureWorkspace(input.jobId, requestedWorkspace)
    : undefined;
  if (managedWorkspace) {
    await context.log(
      `using managed workspace ${managedWorkspace.root} at ${managedWorkspace.revision}`,
    );
  }
  const stages: FactoryJobResult['stages'] = [];
  let lineageThreadId: string | undefined;

  for (const original of ordered) {
    const kind = operationKind(original.kind);
    while (true) {
      const currentJob = await coordinator.loadJob(input.jobId);
      if (currentJob.job.state === 'cancelled') throw new JobCancelledError();
      const operation = operationFrom(currentJob, original.operationId);
      if (operation.state === 'succeeded') break;
      if (operation.state === 'failed' || operation.state === 'cancelled') {
        throw new TerminalOperationError(
          `operation ${operation.operationId} is ${operation.state}`,
        );
      }
      const eligibleAt = new Date(operation.nextEligibleAt);
      if (operation.state === 'retryWait' && eligibleAt.getTime() > Date.now()) {
        await context.log(`waiting until ${eligibleAt.toISOString()} to retry ${kind}`);
        await context.sleepUntil(eligibleAt);
        continue;
      }

      const leaseSeconds = Number(process.env.FACTORY_LEASE_SECONDS ?? DEFAULT_LEASE_SECONDS);
      const lease = await coordinator.claimOperation(operation.operationId, {
        ownerInstanceId: context.ownerInstanceId,
        leaseSeconds,
      });
      if (!lease) {
        await context.log(`operation ${operation.operationId} is not claimable yet`);
        await sleepFor(
          context,
          Number(process.env.FACTORY_CLAIM_POLL_SECONDS ?? DEFAULT_CLAIM_POLL_SECONDS),
        );
        continue;
      }
      const renewer = new LeaseRenewer(
        coordinator,
        lease,
        context.ownerInstanceId,
        leaseSeconds,
      );
      let stageInput: CodexOperationInput | undefined;
      renewer.start();
      try {
        if (lease.selection.operationId !== operation.operationId ||
          lease.selection.operationKind !== operation.kind) {
          throw new TerminalOperationError(
            `factoryd claimed ${lease.selection.operationId}/${lease.selection.operationKind} ` +
            `for requested ${operation.operationId}/${operation.kind}`,
          );
        }

        const checkpointLineage = checkpointThreadId(lease);
        if (operation.ordinal > 0 && !checkpointLineage) {
          throw new TerminalOperationError(
            `later stage ${kind} did not receive the prior operation checkpoint thread`,
          );
        }
        if (lineageThreadId && checkpointLineage !== lineageThreadId) {
          throw new TerminalOperationError(
            `checkpoint lineage changed before ${kind}: expected ${lineageThreadId}, got ${checkpointLineage ?? 'none'}`,
          );
        }

        if (integration && operation.ordinal === 0) {
          await integration.jobStarted(input.jobId, operation);
        }

        if (hasCompletedStageCheckpoint(lease, kind)) {
          const threadId = checkpointLineage as string;
          const sourceCheckpoint = lease.selection.resume.kind === 'fromCheckpoint'
            ? lease.selection.resume.checkpoint
            : undefined;
          const recoveredResult: OperationResult = {
            threadId,
            ...(sourceCheckpoint?.workspaceRevision
              ? { workspaceRevision: sourceCheckpoint.workspaceRevision }
              : {}),
            recoveredFromCheckpoint: true,
          };
          await enforceFactoryStageState({
            coordinator,
            lease,
            stage: kind,
            threadId,
            ...(sourceCheckpoint?.workspaceRoot
              ? { workspaceRoot: sourceCheckpoint.workspaceRoot }
              : {}),
            ...(sourceCheckpoint?.workspaceRevision
              ? { workspaceRevision: sourceCheckpoint.workspaceRevision }
              : {}),
            log: (message) => context.log(message),
          });
          if (integration) {
            await integration.stageCompleted({
              jobId: input.jobId,
              attemptId: lease.attempt.attemptId,
              operation,
              kind,
              result: recoveredResult,
              recoveredFromCheckpoint: true,
            });
            if (operation.ordinal === ordered.length - 1) {
              await integration.jobCompleted(input.jobId, recoveredResult);
            }
          }
          await coordinator.saveCheckpoint({
            attemptId: lease.attempt.attemptId,
            kind: `${kind}.recovered-completion`,
            payload: {
              stage: kind,
              phase: 'recoveredCompletion',
              threadId,
              sourceCheckpointId: lease.selection.resume.kind === 'fromCheckpoint'
                ? lease.selection.resume.checkpoint.checkpointId
                : null,
            },
            ...(sourceCheckpoint?.workspaceRoot
              ? { workspaceRoot: sourceCheckpoint.workspaceRoot }
              : {}),
            ...(sourceCheckpoint?.workspaceRevision
              ? { workspaceRevision: sourceCheckpoint.workspaceRevision }
              : {}),
          });
          await coordinator.completeAttempt(lease.attempt.attemptId);
          lineageThreadId = threadId;
          stages.push({
            operationId: operation.operationId,
            kind,
            threadId,
            recoveredFromCheckpoint: true,
          });
          break;
        }

        let resolvedInput = codexInput(currentJob, operation, managedWorkspace);
        if (resolvedInput.usesManagedWorkspace) {
          managedWorkspace = await coordinator.refreshWorkspaceRevision(input.jobId);
          resolvedInput = codexInput(currentJob, operation, managedWorkspace);
        }
        stageInput = resolvedInput.input;
        await context.log(
          `running ${kind} attempt ${lease.attempt.attemptNumber}/${lease.selection.maxAttempts}`,
        );
        const result = await executeCodexOperation({
          coordinator,
          lease,
          kind,
          input: stageInput,
          correlation: {
            jobId: lease.selection.jobId,
            operationId: lease.selection.operationId,
            attemptId: lease.attempt.attemptId,
            ...(context.workflowRunId ? { workflowRunId: context.workflowRunId } : {}),
            ...(context.taskRunExternalId
              ? { taskRunExternalId: context.taskRunExternalId }
              : {}),
          },
          ...(checkpointLineage || lineageThreadId
            ? { expectedThreadId: checkpointLineage ?? lineageThreadId }
            : {}),
          ...(resolvedInput.usesManagedWorkspace
            ? {
                refreshWorkspaceRevision: async () => {
                  managedWorkspace = await coordinator.refreshWorkspaceRevision(input.jobId);
                  return managedWorkspace.revision;
                },
              }
            : {}),
          isCancelled: async () =>
            (await coordinator.loadJob(input.jobId)).job.state === 'cancelled',
          log: (message) => context.log(message),
        });
        renewer.assertHealthy();
        if (lineageThreadId && result.threadId !== lineageThreadId) {
          throw new TerminalOperationError(
            `stage ${kind} returned unrelated thread ${result.threadId}; expected ${lineageThreadId}`,
          );
        }
        await enforceFactoryStageState({
          coordinator,
          lease,
          stage: kind,
          threadId: result.threadId,
          workspaceRoot: stageInput.cwd,
          ...(result.workspaceRevision
            ? { workspaceRevision: result.workspaceRevision }
            : {}),
          log: (message) => context.log(message),
        });
        if (integration) {
          await integration.stageCompleted({
            jobId: input.jobId,
            attemptId: lease.attempt.attemptId,
            operation,
            kind,
            result,
            recoveredFromCheckpoint: result.recoveredFromCheckpoint,
          });
          if (operation.ordinal === ordered.length - 1) {
            await integration.jobCompleted(input.jobId, result);
          }
        }
        await coordinator.completeAttempt(lease.attempt.attemptId);
        lineageThreadId = result.threadId;
        stages.push({
          operationId: operation.operationId,
          kind,
          threadId: result.threadId,
          ...(result.turnId ? { turnId: result.turnId } : {}),
          recoveredFromCheckpoint: result.recoveredFromCheckpoint,
        });
        break;
      } catch (error) {
        const cancelled = error instanceof JobCancelledError ||
          await coordinator.loadJob(input.jobId)
            .then((job) => job.job.state === 'cancelled')
            .catch(() => false);
        if (cancelled) {
          await context.log(`cancelled ${kind}`);
          throw error instanceof JobCancelledError ? error : new JobCancelledError();
        }
        const canRetry = !(error instanceof TerminalOperationError) &&
          lease.attempt.attemptNumber < lease.selection.maxAttempts;
        if (canRetry) {
          const retryAt = new Date(
            Date.now() + (stageInput?.retryDelaySeconds ?? 30) * 1000,
          );
          await coordinator.failAttempt(lease.attempt.attemptId, {
            disposition: 'retryAt',
            retryAt: retryAt.toISOString(),
            detail: failureDetail(error, operation),
          });
          await context.log(`scheduled ${kind} retry for ${retryAt.toISOString()}`);
          await context.sleepUntil(retryAt);
          continue;
        }
        await coordinator.failAttempt(lease.attempt.attemptId, {
          disposition: 'terminal',
          detail: failureDetail(error, operation),
        });
        throw error;
      } finally {
        renewer.stop();
      }
    }
  }

  return { jobId: input.jobId, state: 'succeeded', stages };
}
