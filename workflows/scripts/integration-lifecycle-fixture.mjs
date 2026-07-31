import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import {
  IntegrationRegistry,
  loadIntegrations,
} from '../../integrations/dist/index.js';
import { executeFactoryJob } from '../dist/factory-job.js';
import { IntegrationLifecyclePublisher } from '../dist/integration-lifecycle.js';

const PLUGINS_ENV = 'FACTORY_INTEGRATION_PLUGINS_JSON';
const NOW = '2026-07-31T00:00:00.000Z';
const ADAPTER_NAME = 'disposable-intake';
const pluginModule = new URL('./fixtures/disposable-intake-plugin.mjs', import.meta.url).href;

const factoryState = {
  decomposition: {
    revision: 1,
    work_units: [
      {
        id: 'design',
        title: 'Design',
        description: 'Define the implementation.',
        depends_on: [],
      },
      {
        id: 'implement',
        title: 'Implement',
        description: 'Build the defined implementation.',
        depends_on: ['design'],
      },
    ],
  },
  progress: {
    work_units: [
      { id: 'design', status: 'completed', progress_summary: 'Design complete.' },
      { id: 'implement', status: 'completed', progress_summary: 'Implementation complete.' },
    ],
  },
  review: {
    verdict: 'request_changes',
    summary: 'One finding requires remediation.',
    findings: [
      {
        id: 'F1',
        severity: 'minor',
        unit_id: 'implement',
        title: 'Clarify behavior',
        evidence: 'The behavior needs one explicit note.',
        recommendation: 'Add the note.',
      },
    ],
  },
  remediation: {
    records: [
      {
        finding_id: 'F1',
        disposition: 'resolved',
        rationale: 'The note was added.',
        unit_id: 'implement',
      },
    ],
  },
};

class RecoveredCheckpointCoordinator {
  constructor(jobId, kinds, threadId) {
    this.jobId = jobId;
    this.threadId = threadId;
    this.attemptCounts = new Map();
    this.attemptOperations = new Map();
    this.claims = [];
    this.completedAttempts = [];
    this.failedAttempts = [];
    this.checkpoints = [];
    this.job = {
      job: {
        jobId,
        kind: 'integration-fixture',
        input: {
          integration: {
            intake: {
              adapter: ADAPTER_NAME,
              externalId: `external-${jobId}`,
              url: `https://intake.invalid/${jobId}`,
            },
          },
        },
        state: 'queued',
        workflowRunId: `hatchet-${jobId}`,
        createdAt: NOW,
        updatedAt: NOW,
      },
      operations: kinds.map((kind, ordinal) => ({
        operationId: `${jobId}-operation-${ordinal}`,
        jobId,
        ordinal,
        kind,
        input: {},
        state: 'ready',
        maxAttempts: 3,
        nextEligibleAt: NOW,
        createdAt: NOW,
        updatedAt: NOW,
      })),
    };
  }

  async loadJob(jobId) {
    assert.equal(jobId, this.jobId);
    return structuredClone(this.job);
  }

  async claimOperation(operationId, request) {
    const operation = this.job.operations.find((candidate) => (
      candidate.operationId === operationId
    ));
    assert.ok(operation);
    assert.notEqual(operation.state, 'succeeded');
    const attemptNumber = (this.attemptCounts.get(operationId) ?? 0) + 1;
    this.attemptCounts.set(operationId, attemptNumber);
    operation.state = 'running';
    this.job.job.state = 'running';
    const attemptId = `${operationId}-attempt-${attemptNumber}`;
    this.attemptOperations.set(attemptId, operation);
    this.claims.push({ operationId, attemptId });
    const sourceAttemptId = `${operationId}-codex-completed`;
    const checkpoint = {
      checkpointId: `${operationId}-completed-checkpoint`,
      attemptId: sourceAttemptId,
      sequence: 1,
      kind: `${operation.kind}.completed`,
      payload: {
        stage: operation.kind,
        phase: 'completed',
        threadId: this.threadId,
        turnStatus: 'completed',
      },
      workspaceRoot: null,
      workspaceRevision: null,
      correlationId: null,
      createdAt: NOW,
    };
    return {
      selection: {
        jobId: this.jobId,
        operationId,
        operationKind: operation.kind,
        cause: attemptNumber === 1 ? 'leaseExpired' : 'retryScheduled',
        previousAttemptId: sourceAttemptId,
        nextAttemptNumber: attemptNumber,
        maxAttempts: operation.maxAttempts,
        resume: { kind: 'fromCheckpoint', checkpoint },
        checkpointCorrelation: null,
      },
      attempt: {
        attemptId,
        operationId,
        attemptNumber,
        state: 'running',
        ownerInstanceId: request.ownerInstanceId,
        leaseExpiresAt: new Date(Date.now() + request.leaseSeconds * 1000).toISOString(),
        recoveryCause: attemptNumber === 1 ? 'leaseExpired' : 'retryScheduled',
        resumesAttemptId: sourceAttemptId,
        resumesCheckpointId: checkpoint.checkpointId,
        failure: null,
        startedAt: NOW,
        finishedAt: null,
      },
    };
  }

  async getThreadState(threadId) {
    assert.equal(threadId, this.threadId);
    return {
      threadId,
      state: structuredClone(factoryState),
      revision: 1,
      createdAt: NOW,
      updatedAt: NOW,
    };
  }

  async saveCheckpoint(checkpoint) {
    const saved = {
      checkpointId: `saved-checkpoint-${this.checkpoints.length + 1}`,
      attemptId: checkpoint.attemptId,
      sequence: this.checkpoints.length + 1,
      kind: checkpoint.kind,
      payload: checkpoint.payload,
      workspaceRoot: checkpoint.workspaceRoot ?? null,
      workspaceRevision: checkpoint.workspaceRevision ?? null,
      correlationId: checkpoint.correlationId ?? null,
      createdAt: NOW,
    };
    this.checkpoints.push(saved);
    return saved;
  }

  async completeAttempt(attemptId) {
    const operation = this.attemptOperations.get(attemptId);
    assert.ok(operation);
    operation.state = 'succeeded';
    this.completedAttempts.push(attemptId);
    if (this.job.operations.every((candidate) => candidate.state === 'succeeded')) {
      this.job.job.state = 'succeeded';
    }
  }

  async failAttempt(attemptId, failure) {
    const operation = this.attemptOperations.get(attemptId);
    assert.ok(operation);
    this.failedAttempts.push({ attemptId, failure });
    if (failure.disposition === 'retryAt') {
      operation.state = 'retryWait';
      operation.nextEligibleAt = failure.retryAt;
    } else {
      operation.state = 'failed';
      this.job.job.state = 'failed';
    }
  }

  makeRetriesEligible() {
    for (const operation of this.job.operations) {
      if (operation.state === 'retryWait') {
        operation.nextEligibleAt = new Date(0).toISOString();
      }
    }
  }
}

function contextFor(coordinator) {
  return {
    ownerInstanceId: 'integration-fixture-worker',
    workflowRunId: `workflow-${coordinator.jobId}`,
    taskRunExternalId: `task-${coordinator.jobId}`,
    async log() {},
    async sleepUntil() {
      coordinator.makeRetriesEligible();
    },
  };
}

function recordsFor(records, externalId) {
  return records.filter((record) => record.reference.externalId === externalId);
}

const temporaryRoot = await mkdtemp(join(tmpdir(), 'factory-integration-fixture-'));
const outputPath = join(temporaryRoot, 'lifecycle.jsonl');
const previousPlugins = process.env[PLUGINS_ENV];

try {
  delete process.env[PLUGINS_ENV];
  assert.equal(await IntegrationLifecyclePublisher.fromJobInput({}), undefined);
  await assert.rejects(
    IntegrationLifecyclePublisher.fromJobInput({
      integration: {
        intake: { adapter: ADAPTER_NAME, externalId: 'requires-opt-in' },
      },
    }),
    new RegExp(`${PLUGINS_ENV} is not configured`),
  );

  const registry = new IntegrationRegistry();
  await loadIntegrations(registry, [{
    module: pluginModule,
    config: { adapterName: ADAPTER_NAME, outputPath },
  }]);
  assert.equal(registry.intake(ADAPTER_NAME).name, ADAPTER_NAME);
  assert.throws(() => registry.sourceHost(ADAPTER_NAME), /source-host .* is not registered/);
  assert.throws(() => registry.artifacts(ADAPTER_NAME), /artifact .* is not registered/);

  const retryJobId = 'integration-retry';
  const failedEventId = `factory:${retryJobId}:${retryJobId}-operation-0:codex.plan.completed`;
  process.env[PLUGINS_ENV] = JSON.stringify([{
    module: pluginModule,
    config: {
      adapterName: ADAPTER_NAME,
      outputPath,
      failOnceEventId: failedEventId,
    },
  }]);

  const fullCoordinator = new RecoveredCheckpointCoordinator(
    'integration-full',
    ['codex.plan', 'codex.execute', 'codex.review', 'codex.remediate'],
    'thread-integration-full',
  );
  const fullResult = await executeFactoryJob(
    { jobId: fullCoordinator.jobId },
    contextFor(fullCoordinator),
    fullCoordinator,
  );
  assert.equal(fullResult.state, 'succeeded');
  assert.ok(fullResult.stages.every((stage) => stage.recoveredFromCheckpoint));

  const retryCoordinator = new RecoveredCheckpointCoordinator(
    retryJobId,
    ['codex.plan'],
    'thread-integration-retry',
  );
  const retryResult = await executeFactoryJob(
    { jobId: retryCoordinator.jobId },
    contextFor(retryCoordinator),
    retryCoordinator,
  );
  assert.equal(retryResult.state, 'succeeded');
  assert.equal(retryResult.stages[0].recoveredFromCheckpoint, true);
  assert.equal(retryCoordinator.claims.length, 2);
  assert.equal(retryCoordinator.failedAttempts.length, 1);
  assert.equal(retryCoordinator.completedAttempts.length, 1);

  const records = (await readFile(outputPath, 'utf8'))
    .trim()
    .split('\n')
    .map((line) => JSON.parse(line));
  const fullRecords = recordsFor(records, 'external-integration-full');
  assert.deepEqual(
    fullRecords.map((record) => record.event.eventId),
    [
      'factory:integration-full:job.started',
      'factory:integration-full:integration-full-operation-0:codex.plan.completed',
      'factory:integration-full:integration-full-operation-1:codex.execute.completed',
      'factory:integration-full:integration-full-operation-2:codex.review.completed',
      'factory:integration-full:integration-full-operation-3:codex.remediate.completed',
      'factory:integration-full:job.completed',
    ],
  );
  assert.deepEqual(
    fullRecords.map((record) => record.event.type),
    [
      'job.started',
      'plan.completed',
      'implementation.completed',
      'review.completed',
      'remediation.completed',
      'job.completed',
    ],
  );
  assert.ok(fullRecords.every((record) => record.outcome === 'delivered'));

  const retryRecords = recordsFor(records, `external-${retryJobId}`);
  const stageAttempts = retryRecords.filter((record) => record.event.eventId === failedEventId);
  assert.equal(stageAttempts.length, 2);
  assert.deepEqual(stageAttempts.map((record) => record.outcome), ['failed', 'delivered']);
  assert.equal(new Set(stageAttempts.map((record) => record.event.eventId)).size, 1);
  assert.notEqual(
    stageAttempts[0].event.execution.attemptId,
    stageAttempts[1].event.execution.attemptId,
  );
  assert.equal(
    retryRecords.filter((record) => record.event.type === 'job.started').length,
    2,
  );
  assert.equal(
    new Set(
      retryRecords
        .filter((record) => record.event.type === 'job.started')
        .map((record) => record.event.eventId),
    ).size,
    1,
  );
  assert.equal(
    retryRecords.filter((record) => record.event.type === 'job.completed').length,
    1,
  );

  console.log(JSON.stringify({
    fixture: 'integration-lifecycle',
    pluginOptIn: true,
    intakeOnly: true,
    fullLifecycleEventIds: fullRecords.map((record) => record.event.eventId),
    retriedEventId: failedEventId,
    deliveryAttempts: stageAttempts.length,
    modelStageRuns: 0,
  }, null, 2));
} finally {
  if (previousPlugins === undefined) delete process.env[PLUGINS_ENV];
  else process.env[PLUGINS_ENV] = previousPlugins;
  await rm(temporaryRoot, { recursive: true, force: true });
}
