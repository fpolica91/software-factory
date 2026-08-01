import type { JsonValue } from '@software-factory/harness-client';
import { CoordinatorClient, CoordinatorHttpError } from './coordinator-client.js';
import {
  FACTORY_STAGE_CONTRACT,
  FactoryStateGateError,
  assertFactoryStageState,
  factoryReviewGeneration,
  factoryReviewParentTurnId,
  factoryReviewParentThreadId,
  factoryReviewTurnId,
  factoryReviewVerdict,
  type FactoryReviewVerdict,
} from './factory-state-gate.js';
import { IntegrationLifecyclePublisher } from './integration-lifecycle.js';
import {
  JobCancelledError,
  TerminalOperationError,
  checkpointThreadId,
  executeCodexOperation,
  hasCompletedStageCheckpoint,
} from './lifecycle.js';
import { deploymentCodexDefaults } from './provider-config.js';
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
const DEFAULT_MAX_REVIEW_CYCLES = 5;
const AUTONOMOUS_JOB_INSTRUCTIONS = [
  'Operate autonomously through the current Factory stage.',
  'Never ask for user input; make reasonable repository-grounded assumptions.',
  'Use the available tools, verify the work, and continue until the stage contract is satisfied.',
].join(' ');

interface ValidatedFactoryState {
  state: FactoryThreadStateDocument;
  revision: number;
  reviewGeneration: number;
  reviewTurnId?: string;
  reviewParentThreadId?: string;
  reviewParentTurnId?: string;
}

async function enforceFactoryStageState(options: {
  coordinator: CoordinatorClient;
  lease: RecoveryLease;
  stage: OperationKind;
  threadId: string;
  workspaceRoot?: string;
  workspaceRevision?: string;
  minimumRevisionExclusive?: number;
  minimumReviewGenerationExclusive?: number;
  expectedReviewTurnId?: string;
  expectedReviewParentTurnId?: string;
  allowLegacyReview?: boolean;
  checkpointMetadata?: Record<string, JsonValue>;
  log(message: string): Promise<void>;
}): Promise<ValidatedFactoryState> {
  const {
    coordinator,
    lease,
    stage,
    threadId,
    workspaceRoot,
    workspaceRevision,
    minimumRevisionExclusive,
    minimumReviewGenerationExclusive,
    expectedReviewTurnId,
    expectedReviewParentTurnId,
    allowLegacyReview,
    checkpointMetadata,
    log,
  } = options;
  let state: FactoryThreadStateDocument;
  let revision = 0;
  let reviewGeneration = 0;
  let reviewTurnId: string | undefined;
  let reviewParentThreadId: string | undefined;
  let reviewParentTurnId: string | undefined;
  try {
    const record = await coordinator.getThreadState(threadId);
    state = record.state;
    revision = record.revision;
  } catch (error) {
    if (!(error instanceof CoordinatorHttpError) || error.status !== 404) throw error;
    state = {};
  }

  try {
    reviewGeneration = factoryReviewGeneration(state);
    reviewTurnId = factoryReviewTurnId(state);
    reviewParentThreadId = factoryReviewParentThreadId(state);
    reviewParentTurnId = factoryReviewParentTurnId(state);
    if (minimumRevisionExclusive !== undefined && revision <= minimumRevisionExclusive) {
      throw new FactoryStateGateError(
        stage,
        `thread state revision ${revision} did not advance beyond ${minimumRevisionExclusive}`,
      );
    }
    if (minimumReviewGenerationExclusive !== undefined &&
      reviewGeneration <= minimumReviewGenerationExclusive) {
      throw new FactoryStateGateError(
        stage,
        `review generation ${reviewGeneration} did not advance beyond ${minimumReviewGenerationExclusive}`,
      );
    }
    if (expectedReviewTurnId !== undefined && reviewTurnId !== expectedReviewTurnId) {
      throw new FactoryStateGateError(
        stage,
        `review was recorded by turn ${reviewTurnId ?? 'none'}, expected ${expectedReviewTurnId}`,
      );
    }
    if (expectedReviewParentTurnId !== undefined &&
      reviewParentTurnId !== expectedReviewParentTurnId) {
      throw new FactoryStateGateError(
        stage,
        `review parent turn is ${reviewParentTurnId ?? 'none'}, expected ${expectedReviewParentTurnId}`,
      );
    }
    if (stage === 'codex.review' && allowLegacyReview !== true &&
      reviewParentThreadId !== threadId) {
      throw new FactoryStateGateError(
        stage,
        `review parent thread is ${reviewParentThreadId ?? 'none'}, expected ${threadId}`,
      );
    }
    assertFactoryStageState(stage, state, { allowLegacyReview });
  } catch (error) {
    if (!(error instanceof FactoryStateGateError)) throw error;
    await coordinator.saveCheckpoint({
      attemptId: lease.attempt.attemptId,
      kind: `${stage}.semantic-gate-failed`,
      payload: {
        ...checkpointMetadata,
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
  return {
    state,
    revision,
    reviewGeneration,
    ...(reviewTurnId ? { reviewTurnId } : {}),
    ...(reviewParentThreadId ? { reviewParentThreadId } : {}),
    ...(reviewParentTurnId ? { reviewParentTurnId } : {}),
  };
}

async function currentFactoryReviewGeneration(
  coordinator: CoordinatorClient,
  threadId: string,
): Promise<number> {
  try {
    return factoryReviewGeneration((await coordinator.getThreadState(threadId)).state);
  } catch (error) {
    if (error instanceof CoordinatorHttpError && error.status === 404) return 0;
    throw error;
  }
}

function maxReviewCycles(): number {
  const value = Number(process.env.FACTORY_MAX_REVIEW_CYCLES ?? DEFAULT_MAX_REVIEW_CYCLES);
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new TerminalOperationError('FACTORY_MAX_REVIEW_CYCLES must be a positive integer');
  }
  return value;
}

function reReviewPrompt(cycle: number): string {
  return [
    `Perform autonomous post-remediation review cycle ${cycle}.`,
    'Inspect the actual diff and run the relevant tests or verification commands.',
    'Call factory_record_review with approve only when the original task is fully satisfied and verification passes.',
    'Otherwise record concrete findings tied to the current Factory work units so another remediation cycle can fix them.',
    'Do not ask the user for input.',
  ].join(' ');
}

function repeatedRemediationPrompt(cycle: number): string {
  return [
    `Perform autonomous remediation cycle ${cycle}.`,
    'Inspect every finding in the current Factory review, fix the underlying code, and run the relevant tests.',
    'Call factory_record_remediation with exactly one disposition for every current finding.',
    'Do not ask the user for input and do not stop at an explanation when a code or test change is required.',
  ].join(' ');
}

function developerInstructionsForStage(
  input: CodexOperationInput,
  currentStage: OperationKind,
  targetStage: OperationKind,
): string {
  const currentContract = FACTORY_STAGE_CONTRACT[currentStage];
  const instructions = input.developerInstructions?.trim() ?? '';
  const base = instructions.endsWith(currentContract)
    ? instructions.slice(0, -currentContract.length).trim()
    : instructions;
  return base
    ? `${base}\n\n${FACTORY_STAGE_CONTRACT[targetStage]}`
    : FACTORY_STAGE_CONTRACT[targetStage];
}

type ReviewLoopResume =
  | { phase: 'remediate'; cycle: number }
  | { phase: 'review'; cycle: number; minimumRevisionExclusive?: number }
  | {
      phase: 'reviewed';
      cycle: number;
      validated: boolean;
      verdict?: FactoryReviewVerdict;
      minimumReviewGenerationExclusive?: number;
      expectedReviewGeneration?: number;
      expectedReviewTurnId?: string;
      expectedReviewParentTurnId?: string;
    };

function reviewLoopCheckpointPayload(lease: RecoveryLease): Record<string, JsonValue> {
  if (lease.selection.resume.kind !== 'fromCheckpoint') return {};
  const { payload } = lease.selection.resume.checkpoint;
  return typeof payload === 'object' && payload !== null && !Array.isArray(payload)
    ? payload as Record<string, JsonValue>
    : {};
}

function reviewLoopCycle(value: JsonValue | undefined, fallback = 1): number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 1
    ? value
    : fallback;
}

function reviewLoopResume(lease: RecoveryLease): ReviewLoopResume {
  if (lease.selection.resume.kind !== 'fromCheckpoint') {
    return { phase: 'remediate', cycle: 1 };
  }
  const checkpoint = lease.selection.resume.checkpoint;
  const payload = reviewLoopCheckpointPayload(lease);
  const cycle = reviewLoopCycle(payload.reviewLoopCycle ?? payload.cycle);
  const step = payload.reviewLoopStep;

  if (step === 'remediation') {
    if (checkpoint.kind !== 'codex.remediate.turn-completed' &&
      checkpoint.kind !== 'codex.remediate.completed') {
      return { phase: 'remediate', cycle };
    }
    const baseline = payload.factoryStateRevisionBefore;
    return {
      phase: 'review',
      cycle,
      ...(typeof baseline === 'number' && Number.isSafeInteger(baseline) && baseline >= 0
        ? { minimumRevisionExclusive: baseline }
        : {}),
    };
  }
  if (step === 'review') {
    if (checkpoint.kind !== 'codex.review.completed') {
      return { phase: 'review', cycle };
    }
    const baseline = payload.factoryReviewGenerationBefore;
    return {
      phase: 'reviewed',
      cycle,
      validated: false,
      ...(typeof baseline === 'number' && Number.isSafeInteger(baseline) && baseline >= 0
        ? { minimumReviewGenerationExclusive: baseline }
        : {}),
    };
  }
  if (checkpoint.kind === 'codex.remediate.cycle-remediated') {
    return { phase: 'review', cycle };
  }
  if (checkpoint.kind === 'codex.remediate.cycle-reviewed') {
    const value = payload.verdict;
    const verdict = value === 'approve' || value === 'request_changes' || value === 'blocked'
      ? value
      : undefined;
    const reviewGeneration = payload.reviewGeneration;
    const reviewTurnId = payload.reviewTurnId;
    const reviewParentTurnId = payload.reviewParentTurnId;
    return {
      phase: 'reviewed',
      cycle,
      validated: true,
      ...(verdict ? { verdict } : {}),
      ...(typeof reviewGeneration === 'number' && Number.isSafeInteger(reviewGeneration) &&
        reviewGeneration >= 1
        ? { expectedReviewGeneration: reviewGeneration }
        : {}),
      ...(typeof reviewTurnId === 'string' && reviewTurnId !== ''
        ? { expectedReviewTurnId: reviewTurnId }
        : {}),
      ...(typeof reviewParentTurnId === 'string' && reviewParentTurnId !== ''
        ? { expectedReviewParentTurnId: reviewParentTurnId }
        : {}),
    };
  }
  if (checkpoint.kind === 'codex.remediate.completed' &&
    payload.reviewLoopComplete !== true) {
    return { phase: 'review', cycle };
  }
  return { phase: 'remediate', cycle: 1 };
}

function resultFromResumeCheckpoint(lease: RecoveryLease): OperationResult {
  if (lease.selection.resume.kind !== 'fromCheckpoint') {
    throw new TerminalOperationError('review-loop recovery requires a checkpoint');
  }
  const checkpoint = lease.selection.resume.checkpoint;
  const payload = reviewLoopCheckpointPayload(lease);
  const threadId = typeof payload.threadId === 'string'
    ? payload.threadId
    : checkpointThreadId(lease);
  if (!threadId) throw new TerminalOperationError('review-loop checkpoint has no Codex thread');
  return {
    threadId,
    ...(typeof payload.turnId === 'string' ? { turnId: payload.turnId } : {}),
    ...(checkpoint.workspaceRevision
      ? { workspaceRevision: checkpoint.workspaceRevision }
      : {}),
    recoveredFromCheckpoint: true,
  };
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
    approvalPolicy: 'never',
    sandbox: 'danger-full-access',
    developerInstructions: AUTONOMOUS_JOB_INSTRUCTIONS,
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
          const sourcePayload = sourceCheckpoint &&
            typeof sourceCheckpoint.payload === 'object' &&
            sourceCheckpoint.payload !== null &&
            !Array.isArray(sourceCheckpoint.payload)
            ? sourceCheckpoint.payload as Record<string, JsonValue>
            : {};
          const recoveredResult: OperationResult = {
            threadId,
            ...(typeof sourcePayload.turnId === 'string'
              ? { turnId: sourcePayload.turnId }
              : {}),
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
            ...(kind === 'codex.review' &&
              typeof sourcePayload.factoryReviewGenerationBefore === 'number'
              ? {
                  minimumReviewGenerationExclusive:
                    sourcePayload.factoryReviewGenerationBefore,
                }
              : {}),
            ...(kind === 'codex.review'
              ? {
                  expectedReviewParentTurnId: typeof sourcePayload.turnId === 'string'
                    ? sourcePayload.turnId
                    : '',
                }
              : {}),
            log: (message) => context.log(message),
          });
          if (kind === 'codex.remediate') {
            const reviewed = await enforceFactoryStageState({
              coordinator,
              lease,
              stage: 'codex.review',
              threadId,
              ...(sourceCheckpoint?.workspaceRoot
                ? { workspaceRoot: sourceCheckpoint.workspaceRoot }
                : {}),
              ...(sourceCheckpoint?.workspaceRevision
                ? { workspaceRevision: sourceCheckpoint.workspaceRevision }
                : {}),
              ...(typeof sourcePayload.reviewedReviewTurnId === 'string'
                ? { expectedReviewTurnId: sourcePayload.reviewedReviewTurnId }
                : {}),
              ...(typeof sourcePayload.reviewedReviewParentTurnId === 'string'
                ? {
                    expectedReviewParentTurnId:
                      sourcePayload.reviewedReviewParentTurnId,
                  }
                : {}),
              log: (message) => context.log(message),
            });
            const recoveredVerdict = factoryReviewVerdict(reviewed.state);
            if (recoveredVerdict !== 'approve') {
              throw new TerminalOperationError(
                `completed remediation checkpoint has review verdict ${recoveredVerdict}`,
              );
            }
            const reviewedStateRevision = sourcePayload.reviewedStateRevision;
            if (typeof reviewedStateRevision === 'number' &&
              reviewed.revision !== reviewedStateRevision) {
              throw new TerminalOperationError(
                `completed remediation checkpoint reviewed state revision ${reviewedStateRevision}, current revision is ${reviewed.revision}`,
              );
            }
            const reviewedReviewGeneration = sourcePayload.reviewedReviewGeneration;
            if (typeof reviewedReviewGeneration === 'number' &&
              reviewed.reviewGeneration !== reviewedReviewGeneration) {
              throw new TerminalOperationError(
                `completed remediation checkpoint reviewed generation ${reviewedReviewGeneration}, current generation is ${reviewed.reviewGeneration}`,
              );
            }
            const reviewedReviewTurnId = sourcePayload.reviewedReviewTurnId;
            if (typeof reviewedReviewTurnId === 'string' &&
              reviewed.reviewTurnId !== reviewedReviewTurnId) {
              throw new TerminalOperationError(
                `completed remediation checkpoint reviewed turn ${reviewedReviewTurnId}, current review turn is ${reviewed.reviewTurnId ?? 'none'}`,
              );
            }
            const reviewedReviewParentTurnId = sourcePayload.reviewedReviewParentTurnId;
            if (typeof reviewedReviewParentTurnId === 'string' &&
              reviewed.reviewParentTurnId !== reviewedReviewParentTurnId) {
              throw new TerminalOperationError(
                `completed remediation checkpoint reviewed parent turn ${reviewedReviewParentTurnId}, current review parent turn is ${reviewed.reviewParentTurnId ?? 'none'}`,
              );
            }
          }
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
            kind: `${kind}.completed`,
            payload: {
              ...sourcePayload,
              stage: kind,
              phase: 'recoveredCompletion',
              threadId,
              turnStatus: 'completed',
              recoveredFromCheckpoint: true,
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
        const correlation = {
          jobId: lease.selection.jobId,
          operationId: lease.selection.operationId,
          attemptId: lease.attempt.attemptId,
          ...(context.workflowRunId ? { workflowRunId: context.workflowRunId } : {}),
          ...(context.taskRunExternalId
            ? { taskRunExternalId: context.taskRunExternalId }
            : {}),
        };
        const refreshWorkspaceRevision = resolvedInput.usesManagedWorkspace
          ? async () => {
              managedWorkspace = await coordinator.refreshWorkspaceRevision(input.jobId);
              return managedWorkspace.revision;
            }
          : undefined;
        const isCancelled = async () =>
          (await coordinator.loadJob(input.jobId)).job.state === 'cancelled';
        const runCodexStage = async (
          runKind: OperationKind,
          runInput: CodexOperationInput,
          expectedThreadId?: string,
          checkpointMetadata?: Record<string, JsonValue>,
          completionCheckpointKind?: string,
        ): Promise<OperationResult> => {
          const output = await executeCodexOperation({
            coordinator,
            lease,
            kind: runKind,
            input: runInput,
            correlation,
            ...(checkpointMetadata ? { checkpointMetadata } : {}),
            ...(completionCheckpointKind ? { completionCheckpointKind } : {}),
            ...(expectedThreadId ? { expectedThreadId } : {}),
            ...(refreshWorkspaceRevision ? { refreshWorkspaceRevision } : {}),
            isCancelled,
            log: (message) => context.log(message),
          });
          renewer.assertHealthy();
          if (lineageThreadId && output.threadId !== lineageThreadId) {
            throw new TerminalOperationError(
              `stage ${runKind} returned unrelated thread ${output.threadId}; expected ${lineageThreadId}`,
            );
          }
          return output;
        };

        let result: OperationResult;
        if (kind !== 'codex.remediate') {
          const expectedThreadId = checkpointLineage ?? lineageThreadId;
          const reviewGenerationBefore = kind === 'codex.review' && expectedThreadId
            ? await currentFactoryReviewGeneration(coordinator, expectedThreadId)
            : undefined;
          result = await runCodexStage(
            kind,
            stageInput,
            expectedThreadId,
            reviewGenerationBefore === undefined
              ? undefined
              : { factoryReviewGenerationBefore: reviewGenerationBefore },
          );
          await enforceFactoryStageState({
            coordinator,
            lease,
            stage: kind,
            threadId: result.threadId,
            workspaceRoot: stageInput.cwd,
            ...(result.workspaceRevision
              ? { workspaceRevision: result.workspaceRevision }
              : {}),
            ...(reviewGenerationBefore === undefined
              ? {}
              : { minimumReviewGenerationExclusive: reviewGenerationBefore }),
            ...(kind === 'codex.review'
              ? { expectedReviewParentTurnId: result.turnId ?? '' }
              : {}),
            ...(reviewGenerationBefore === undefined
              ? {}
              : {
                  checkpointMetadata: {
                    factoryReviewGenerationBefore: reviewGenerationBefore,
                  },
                }),
            log: (message) => context.log(message),
          });
        } else {
          const maximumReviewCycles = maxReviewCycles();
          const resumed = reviewLoopResume(lease);
          let phase: ReviewLoopResume['phase'] = resumed.phase;
          let cycle = resumed.cycle;
          let reviewCycles = Math.max(0, cycle - 1);
          let verdict: FactoryReviewVerdict | undefined = resumed.phase === 'reviewed'
            ? resumed.verdict
            : undefined;
          let factoryState: ValidatedFactoryState | undefined;
          let loopResult: OperationResult | undefined = resumed.phase === 'remediate'
            ? undefined
            : resultFromResumeCheckpoint(lease);

          if (resumed.phase === 'reviewed') {
            if (!loopResult) {
              throw new TerminalOperationError('review-loop checkpoint has no result');
            }
            factoryState = await enforceFactoryStageState({
              coordinator,
              lease,
              stage: 'codex.review',
              threadId: loopResult.threadId,
              workspaceRoot: stageInput.cwd,
              ...(!resumed.validated &&
                resumed.minimumReviewGenerationExclusive !== undefined
                ? {
                    minimumReviewGenerationExclusive:
                      resumed.minimumReviewGenerationExclusive,
                  }
                : {}),
              ...(resumed.validated
                ? { expectedReviewTurnId: resumed.expectedReviewTurnId ?? '' }
                : {}),
              expectedReviewParentTurnId: resumed.validated
                ? (resumed.expectedReviewParentTurnId ?? '')
                : (loopResult.turnId ?? ''),
              checkpointMetadata: {
                reviewLoopStep: 'review',
                reviewLoopCycle: cycle,
                ...(!resumed.validated &&
                  resumed.minimumReviewGenerationExclusive !== undefined
                  ? {
                      factoryReviewGenerationBefore:
                        resumed.minimumReviewGenerationExclusive,
                    }
                  : {}),
              },
              log: (message) => context.log(message),
            });
            const persistedVerdict = factoryReviewVerdict(factoryState.state);
            if (resumed.validated && resumed.expectedReviewGeneration !== undefined &&
              factoryState.reviewGeneration !== resumed.expectedReviewGeneration) {
              throw new TerminalOperationError(
                `review-loop checkpoint generation ${resumed.expectedReviewGeneration} does not match current review generation ${factoryState.reviewGeneration}`,
              );
            }
            if (verdict && verdict !== persistedVerdict) {
              throw new TerminalOperationError(
                `review-loop checkpoint verdict ${verdict} does not match Factory state ${persistedVerdict}`,
              );
            }
            verdict = persistedVerdict;
            reviewCycles = cycle;
            if (!resumed.validated) {
              await coordinator.saveCheckpoint({
                attemptId: lease.attempt.attemptId,
                kind: 'codex.remediate.cycle-reviewed',
                payload: {
                  stage: kind,
                  phase: 'cycleReviewed',
                  threadId: loopResult.threadId,
                  cycle,
                  verdict,
                  stateRevision: factoryState.revision,
                  reviewGeneration: factoryState.reviewGeneration,
                  reviewTurnId: factoryState.reviewTurnId ?? null,
                  reviewParentTurnId: factoryState.reviewParentTurnId ?? null,
                  factoryState: factoryState.state as unknown as JsonValue,
                },
                workspaceRoot: stageInput.cwd,
                ...(loopResult.workspaceRevision
                  ? { workspaceRevision: loopResult.workspaceRevision }
                  : {}),
              });
            }
          }

          while (true) {
            if (phase === 'reviewed') {
              if (verdict === 'approve') break;
              if (cycle >= maximumReviewCycles) {
                throw new TerminalOperationError(
                  `review still requires changes after ${maximumReviewCycles} remediation cycles`,
                );
              }
              cycle += 1;
              phase = 'remediate';
              continue;
            }

            if (phase === 'remediate') {
              if (cycle > maximumReviewCycles) {
                throw new TerminalOperationError(
                  `review still requires changes after ${maximumReviewCycles} remediation cycles`,
                );
              }
              const threadId = loopResult?.threadId ?? checkpointLineage ?? lineageThreadId;
              if (!threadId) {
                throw new TerminalOperationError('remediation stage has no Codex thread lineage');
              }
              const reviewBeforeRemediation = await enforceFactoryStageState({
                coordinator,
                lease,
                stage: 'codex.review',
                threadId,
                workspaceRoot: stageInput.cwd,
                allowLegacyReview: true,
                log: (message) => context.log(message),
              });
              const verdictBeforeRemediation = factoryReviewVerdict(
                reviewBeforeRemediation.state,
              );
              if (verdictBeforeRemediation === 'approve') {
                loopResult ??= resultFromResumeCheckpoint(lease);
                factoryState = reviewBeforeRemediation;
                if (reviewBeforeRemediation.reviewGeneration > 0 &&
                  reviewBeforeRemediation.reviewParentThreadId === threadId &&
                  reviewBeforeRemediation.reviewParentTurnId) {
                  verdict = 'approve';
                  reviewCycles = Math.max(0, cycle - 1);
                  break;
                }
                phase = 'review';
              } else {
                const remediationInput: CodexOperationInput = cycle === 1
                  ? stageInput
                  : {
                      ...stageInput,
                      prompt: repeatedRemediationPrompt(cycle),
                      developerInstructions: developerInstructionsForStage(
                        stageInput,
                        kind,
                        'codex.remediate',
                      ),
                      ...(loopResult?.workspaceRevision
                        ? { workspaceRevision: loopResult.workspaceRevision }
                        : {}),
                    };
                loopResult = await runCodexStage(
                  'codex.remediate',
                  remediationInput,
                  threadId,
                  {
                    reviewLoopStep: 'remediation',
                    reviewLoopCycle: cycle,
                    factoryStateRevisionBefore: reviewBeforeRemediation.revision,
                  },
                  'codex.remediate.turn-completed',
                );
                factoryState = await enforceFactoryStageState({
                  coordinator,
                  lease,
                  stage: 'codex.remediate',
                  threadId: loopResult.threadId,
                  workspaceRoot: stageInput.cwd,
                  ...(loopResult.workspaceRevision
                    ? { workspaceRevision: loopResult.workspaceRevision }
                    : {}),
                  minimumRevisionExclusive: reviewBeforeRemediation.revision,
                  checkpointMetadata: {
                    reviewLoopStep: 'remediation',
                    reviewLoopCycle: cycle,
                    factoryStateRevisionBefore: reviewBeforeRemediation.revision,
                  },
                  log: (message) => context.log(message),
                });
                await coordinator.saveCheckpoint({
                  attemptId: lease.attempt.attemptId,
                  kind: 'codex.remediate.cycle-remediated',
                  payload: {
                    stage: kind,
                    phase: 'cycleRemediated',
                    threadId: loopResult.threadId,
                    cycle,
                    stateRevision: factoryState.revision,
                    factoryState: factoryState.state as unknown as JsonValue,
                  },
                  workspaceRoot: stageInput.cwd,
                  ...(loopResult.workspaceRevision
                    ? { workspaceRevision: loopResult.workspaceRevision }
                    : {}),
                });
                phase = 'review';
              }
            } else {
              loopResult ??= resultFromResumeCheckpoint(lease);
              factoryState = await enforceFactoryStageState({
                coordinator,
                lease,
                stage: 'codex.remediate',
                threadId: loopResult.threadId,
                workspaceRoot: stageInput.cwd,
                ...('minimumRevisionExclusive' in resumed &&
                  resumed.minimumRevisionExclusive !== undefined
                  ? { minimumRevisionExclusive: resumed.minimumRevisionExclusive }
                  : {}),
                checkpointMetadata: {
                  reviewLoopStep: 'remediation',
                  reviewLoopCycle: cycle,
                  ...('minimumRevisionExclusive' in resumed &&
                    resumed.minimumRevisionExclusive !== undefined
                    ? { factoryStateRevisionBefore: resumed.minimumRevisionExclusive }
                    : {}),
                },
                log: (message) => context.log(message),
              });
            }

            const reviewGenerationBefore = await currentFactoryReviewGeneration(
              coordinator,
              loopResult.threadId,
            );
            const reviewInput: CodexOperationInput = {
              ...stageInput,
              prompt: reReviewPrompt(cycle),
              developerInstructions: developerInstructionsForStage(
                stageInput,
                kind,
                'codex.review',
              ),
              ...(loopResult.workspaceRevision
                ? { workspaceRevision: loopResult.workspaceRevision }
                : {}),
            };
            loopResult = await runCodexStage(
              'codex.review',
              reviewInput,
              loopResult.threadId,
              {
                reviewLoopStep: 'review',
                reviewLoopCycle: cycle,
                factoryReviewGenerationBefore: reviewGenerationBefore,
              },
            );
            factoryState = await enforceFactoryStageState({
              coordinator,
              lease,
              stage: 'codex.review',
              threadId: loopResult.threadId,
              workspaceRoot: stageInput.cwd,
              ...(loopResult.workspaceRevision
                ? { workspaceRevision: loopResult.workspaceRevision }
                : {}),
              minimumReviewGenerationExclusive: reviewGenerationBefore,
              expectedReviewParentTurnId: loopResult.turnId ?? '',
              checkpointMetadata: {
                reviewLoopStep: 'review',
                reviewLoopCycle: cycle,
                factoryReviewGenerationBefore: reviewGenerationBefore,
              },
              log: (message) => context.log(message),
            });
            verdict = factoryReviewVerdict(factoryState.state);
            reviewCycles = cycle;
            await coordinator.saveCheckpoint({
              attemptId: lease.attempt.attemptId,
              kind: 'codex.remediate.cycle-reviewed',
              payload: {
                stage: kind,
                phase: 'cycleReviewed',
                threadId: loopResult.threadId,
                cycle,
                verdict,
                stateRevision: factoryState.revision,
                reviewGeneration: factoryState.reviewGeneration,
                reviewTurnId: factoryState.reviewTurnId ?? null,
                reviewParentTurnId: factoryState.reviewParentTurnId ?? null,
                factoryState: factoryState.state as unknown as JsonValue,
              },
              workspaceRoot: stageInput.cwd,
              ...(loopResult.workspaceRevision
                ? { workspaceRevision: loopResult.workspaceRevision }
                : {}),
            });
            phase = 'reviewed';
          }

          if (!loopResult || verdict !== 'approve') {
            throw new TerminalOperationError('review/remediation loop ended without approval');
          }
          const approvedReviewTurnId = factoryState?.reviewTurnId;
          const approvedReviewParentTurnId = factoryState?.reviewParentTurnId;
          result = loopResult;
          factoryState = await enforceFactoryStageState({
            coordinator,
            lease,
            stage: 'codex.review',
            threadId: result.threadId,
            workspaceRoot: stageInput.cwd,
            ...(result.workspaceRevision
              ? { workspaceRevision: result.workspaceRevision }
              : {}),
            expectedReviewTurnId: approvedReviewTurnId ?? '',
            expectedReviewParentTurnId: approvedReviewParentTurnId ?? '',
            log: (message) => context.log(message),
          });
          verdict = factoryReviewVerdict(factoryState.state);
          if (verdict !== 'approve') {
            throw new TerminalOperationError(
              `review/remediation loop cannot finalize with verdict ${verdict}`,
            );
          }
          const completion = await coordinator.saveCheckpoint({
            attemptId: lease.attempt.attemptId,
            kind: `${kind}.completed`,
            payload: {
              stage: kind,
              mode: 'normal',
              phase: 'completed',
              threadId: result.threadId,
              turnStatus: 'completed',
              reviewLoopComplete: true,
              finalReviewVerdict: verdict,
              reviewCycles,
              reviewedStateRevision: factoryState.revision,
              reviewedReviewGeneration: factoryState.reviewGeneration,
              reviewedReviewTurnId: factoryState.reviewTurnId ?? null,
              reviewedReviewParentTurnId: factoryState.reviewParentTurnId ?? null,
              ...(result.turnId ? { turnId: result.turnId } : {}),
              ...(result.turn ? { turn: result.turn as unknown as JsonValue } : {}),
            },
            workspaceRoot: stageInput.cwd,
            ...(result.workspaceRevision
              ? { workspaceRevision: result.workspaceRevision }
              : {}),
          });
          await context.log(
            `completed autonomous review/remediation loop after ${reviewCycles} re-review cycles as ${completion.checkpointId}`,
          );
        }
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
