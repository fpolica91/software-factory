import type {
  FactoryCorrelation,
  FactoryRawServerRequest,
  FactoryRawServerResponse,
  JsonValue,
} from '@software-factory/harness-client';
import type { Personality } from '@software-factory/harness-client/codex-v2';
import type {
  AskForApproval,
  SandboxMode,
  Turn,
} from '@software-factory/harness-client/codex-v2/v2';

export const OPERATION_KINDS = [
  'codex.plan',
  'codex.execute',
  'codex.review',
  'codex.remediate',
] as const;

export type OperationKind = (typeof OPERATION_KINDS)[number];
export type JobState = 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled';
export type OperationState = 'ready' | 'running' | 'retryWait' | 'succeeded' | 'failed' | 'cancelled';
export type AttemptState = 'running' | 'succeeded' | 'failed' | 'abandoned';
export type RecoveryCause = 'newOperation' | 'retryScheduled' | 'leaseExpired';

export interface OperationDefinition {
  kind: string;
  input: JsonValue;
  maxAttempts: number;
}

export interface JobDefinition {
  kind: string;
  input: JsonValue;
  workflowRunId?: string;
  operations: OperationDefinition[];
}

export interface JobRecord {
  jobId: string;
  kind: string;
  input: JsonValue;
  state: JobState;
  workflowRunId: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface OperationRecord {
  operationId: string;
  jobId: string;
  ordinal: number;
  kind: string;
  input: JsonValue;
  state: OperationState;
  maxAttempts: number;
  nextEligibleAt: string;
  createdAt: string;
  updatedAt: string;
}

export interface DurableJob {
  job: JobRecord;
  operations: OperationRecord[];
}

export interface FactoryWorkspaceRequest {
  repository: string;
  baseRef?: string;
}

export interface WorkspaceRecord {
  jobId: string;
  repository: string;
  baseRef: string;
  branchName: string;
  root: string;
  revision: string;
  state: 'active' | 'removed';
  createdAt: string;
  updatedAt: string;
}

export interface AttemptRecord {
  attemptId: string;
  operationId: string;
  attemptNumber: number;
  state: AttemptState;
  ownerInstanceId: string;
  leaseExpiresAt: string;
  recoveryCause: RecoveryCause;
  resumesAttemptId: string | null;
  resumesCheckpointId: string | null;
  failure: JsonValue | null;
  startedAt: string;
  finishedAt: string | null;
}

export interface DurableCorrelationRecord {
  correlationId: string;
  correlation: FactoryCorrelation;
  observedAt: string;
}

export type PendingRequestState = 'pending' | 'resolved' | 'inactive';

export interface NewPendingRequest {
  attemptId: string;
  request: FactoryRawServerRequest;
}

export interface PendingRequestResolution {
  response: FactoryRawServerResponse;
}

export interface PendingRequestRecord {
  pendingRequestId: string;
  jobId: string;
  operationId: string;
  attemptId: string;
  request: FactoryRawServerRequest;
  state: PendingRequestState;
  response: FactoryRawServerResponse | null;
  createdAt: string;
  resolvedAt: string | null;
}

export interface NewCheckpoint {
  attemptId: string;
  kind: string;
  payload: JsonValue;
  workspaceRoot?: string;
  workspaceRevision?: string;
  correlationId?: string;
}

export interface CheckpointRecord {
  checkpointId: string;
  attemptId: string;
  sequence: number;
  kind: string;
  payload: JsonValue;
  workspaceRoot: string | null;
  workspaceRevision: string | null;
  correlationId: string | null;
  createdAt: string;
}

export interface StageCheckpointRecord {
  operationId: string;
  ordinal: number;
  operationKind: string;
  checkpoint: CheckpointRecord;
}

export type ResumeStrategy =
  | { kind: 'fresh' }
  | { kind: 'fromCheckpoint'; checkpoint: CheckpointRecord };

export interface RecoverySelection {
  jobId: string;
  operationId: string;
  operationKind: string;
  cause: RecoveryCause;
  previousAttemptId: string | null;
  nextAttemptNumber: number;
  maxAttempts: number;
  resume: ResumeStrategy;
  checkpointCorrelation: DurableCorrelationRecord | null;
}

export interface RecoveryLease {
  selection: RecoverySelection;
  attempt: AttemptRecord;
}

export type AttemptFailure =
  | { disposition: 'retryAt'; retryAt: string; detail: JsonValue }
  | { disposition: 'terminal'; detail: JsonValue };

export interface FactoryThreadStateDocument {
  decomposition?: JsonValue;
  progress?: JsonValue;
  review?: JsonValue;
  remediation?: JsonValue;
  subagents?: JsonValue;
}

export interface FactoryThreadStateRecord {
  threadId: string;
  state: FactoryThreadStateDocument;
  revision: number;
  createdAt: string;
  updatedAt: string;
}

export interface CodexOperationInput {
  cwd: string;
  prompt: string;
  runtimePath?: string;
  runtimeArgs?: string[];
  codexHome?: string;
  env?: Record<string, string>;
  model?: string;
  modelProvider?: string;
  config?: Record<string, JsonValue>;
  approvalPolicy?: AskForApproval;
  sandbox?: SandboxMode;
  personality?: Personality;
  developerInstructions?: string;
  outputSchema?: JsonValue;
  workspaceRevision?: string;
  clientUserMessageId?: string;
  turnTimeoutSeconds?: number;
  retryDelaySeconds?: number;
}

export interface FactoryJobInput {
  [key: string]: JsonValue;
  jobId: string;
}

export interface FactoryJobResult {
  [key: string]: JsonValue;
  jobId: string;
  state: 'succeeded';
  stages: Array<{
    operationId: string;
    kind: OperationKind;
    threadId: string;
    turnId?: string;
    recoveredFromCheckpoint: boolean;
  }>;
}

export interface OperationResult {
  threadId: string;
  turnId?: string;
  turn?: Turn;
  workspaceRevision?: string;
  recoveredFromCheckpoint: boolean;
}

export interface FactoryJobContext {
  ownerInstanceId: string;
  workflowRunId?: string;
  taskRunExternalId?: string;
  log(message: string): Promise<void>;
  sleepUntil(wakeAt: Date): Promise<void>;
}
