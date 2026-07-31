import type { FactoryCorrelation } from '@software-factory/harness-client';
import type {
  AttemptFailure,
  AttemptRecord,
  DurableCorrelationRecord,
  DurableJob,
  FactoryThreadStateDocument,
  FactoryThreadStateRecord,
  JobDefinition,
  NewCheckpoint,
  NewPendingRequest,
  CheckpointRecord,
  PendingRequestRecord,
  PendingRequestResolution,
  RecoveryLease,
  StageCheckpointRecord,
  FactoryWorkspaceRequest,
  WorkspaceRecord,
} from './types.js';

export class CoordinatorHttpError extends Error {
  constructor(
    readonly status: number,
    readonly body: string,
    message: string,
  ) {
    super(message);
    this.name = 'CoordinatorHttpError';
  }
}

export class CoordinatorClient {
  readonly #baseUrl: string;

  constructor(baseUrl = process.env.FACTORYD_URL ?? 'http://127.0.0.1:8787/v1') {
    this.#baseUrl = baseUrl.replace(/\/$/, '');
  }

  createJob(definition: JobDefinition): Promise<DurableJob> {
    return this.#request('POST', '/jobs', definition);
  }

  loadJob(jobId: string): Promise<DurableJob> {
    return this.#request('GET', `/jobs/${encodeURIComponent(jobId)}`);
  }

  listActiveJobs(): Promise<DurableJob[]> {
    return this.#request('GET', '/jobs/active');
  }

  cancelJob(jobId: string): Promise<DurableJob> {
    return this.#request('POST', `/jobs/${encodeURIComponent(jobId)}/cancel`);
  }

  listStageCheckpoints(jobId: string): Promise<StageCheckpointRecord[]> {
    return this.#request(
      'GET',
      `/jobs/${encodeURIComponent(jobId)}/stage-checkpoints`,
    );
  }

  listJobAttempts(jobId: string): Promise<AttemptRecord[]> {
    return this.#request('GET', `/jobs/${encodeURIComponent(jobId)}/attempts`);
  }

  ensureWorkspace(
    jobId: string,
    request: FactoryWorkspaceRequest,
  ): Promise<WorkspaceRecord> {
    return this.#request('PUT', `/jobs/${encodeURIComponent(jobId)}/workspace`, request);
  }

  loadWorkspace(jobId: string): Promise<WorkspaceRecord> {
    return this.#request('GET', `/jobs/${encodeURIComponent(jobId)}/workspace`);
  }

  refreshWorkspaceRevision(jobId: string): Promise<WorkspaceRecord> {
    return this.#request('POST', `/jobs/${encodeURIComponent(jobId)}/workspace/revision`);
  }

  removeWorkspace(jobId: string): Promise<WorkspaceRecord> {
    return this.#request('DELETE', `/jobs/${encodeURIComponent(jobId)}/workspace`);
  }

  claimOperation(
    operationId: string,
    request: { ownerInstanceId: string; leaseSeconds: number },
  ): Promise<RecoveryLease | undefined> {
    return this.#requestOptional(
      'POST',
      `/operations/${encodeURIComponent(operationId)}/claim`,
      request,
    );
  }

  claimRecovery(request: {
    jobId?: string;
    ownerInstanceId: string;
    leaseSeconds: number;
  }): Promise<RecoveryLease | undefined> {
    return this.#requestOptional('POST', '/recoveries/claim', request);
  }

  appendCorrelation(correlation: FactoryCorrelation): Promise<DurableCorrelationRecord> {
    return this.#request('POST', '/correlations', correlation);
  }

  registerPendingRequest(pending: NewPendingRequest): Promise<PendingRequestRecord> {
    return this.#request('POST', '/pending-requests', pending);
  }

  listPendingRequests(jobId?: string): Promise<PendingRequestRecord[]> {
    const query = jobId === undefined ? '' : `?jobId=${encodeURIComponent(jobId)}`;
    return this.#request('GET', `/pending-requests${query}`);
  }

  loadPendingRequest(pendingRequestId: string): Promise<PendingRequestRecord> {
    return this.#request(
      'GET',
      `/pending-requests/${encodeURIComponent(pendingRequestId)}`,
    );
  }

  resolvePendingRequest(
    pendingRequestId: string,
    resolution: PendingRequestResolution,
  ): Promise<PendingRequestRecord> {
    return this.#request(
      'POST',
      `/pending-requests/${encodeURIComponent(pendingRequestId)}/resolve`,
      resolution,
    );
  }

  saveCheckpoint(checkpoint: NewCheckpoint): Promise<CheckpointRecord> {
    return this.#request('POST', '/checkpoints', checkpoint);
  }

  async completeAttempt(attemptId: string): Promise<void> {
    await this.#requestOptional('POST', `/attempts/${encodeURIComponent(attemptId)}/complete`);
  }

  async failAttempt(attemptId: string, failure: AttemptFailure): Promise<void> {
    await this.#requestOptional(
      'POST',
      `/attempts/${encodeURIComponent(attemptId)}/fail`,
      failure,
    );
  }

  renewAttempt(
    attemptId: string,
    request: { ownerInstanceId: string; leaseSeconds: number },
  ): Promise<AttemptRecord> {
    return this.#request(
      'POST',
      `/attempts/${encodeURIComponent(attemptId)}/renew`,
      request,
    );
  }

  getThreadState(threadId: string): Promise<FactoryThreadStateRecord> {
    return this.#request('GET', `/threads/${encodeURIComponent(threadId)}/state`);
  }

  putThreadState(
    threadId: string,
    state: FactoryThreadStateDocument,
  ): Promise<FactoryThreadStateRecord> {
    return this.#request('PUT', `/threads/${encodeURIComponent(threadId)}/state`, state);
  }

  async #request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const response = await this.#fetch(method, path, body);
    if (response.status === 204) {
      throw new CoordinatorHttpError(204, '', `${method} ${path} returned no content`);
    }
    return response.json() as Promise<T>;
  }

  async #requestOptional<T>(method: string, path: string, body?: unknown): Promise<T | undefined> {
    const response = await this.#fetch(method, path, body);
    if (response.status === 204) return undefined;
    return response.json() as Promise<T>;
  }

  async #fetch(method: string, path: string, body?: unknown): Promise<Response> {
    const response = await fetch(`${this.#baseUrl}${path}`, {
      method,
      ...(body === undefined
        ? {}
        : {
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify(body),
          }),
    });
    if (response.ok) return response;
    const responseBody = await response.text();
    throw new CoordinatorHttpError(
      response.status,
      responseBody,
      `${method} ${path} failed with ${response.status}${responseBody ? `: ${responseBody}` : ''}`,
    );
  }
}
