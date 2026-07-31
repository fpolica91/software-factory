import assert from 'node:assert/strict';

import { instantiateFactoryJobTemplate } from '../dist/scheduled-job.js';

const now = '2026-07-31T00:00:00.000Z';
const template = {
  job: {
    jobId: 'template-job',
    kind: 'software-change',
    input: {
      cwd: '/workspace/project',
      prompt: 'Implement the scheduled change.',
    },
    state: 'succeeded',
    workflowRunId: 'template-original-run',
    createdAt: now,
    updatedAt: now,
  },
  operations: [
    {
      operationId: 'template-execute',
      jobId: 'template-job',
      ordinal: 1,
      kind: 'codex.execute',
      input: { prompt: 'Execute the plan.' },
      state: 'succeeded',
      maxAttempts: 5,
      nextEligibleAt: now,
      createdAt: now,
      updatedAt: now,
    },
    {
      operationId: 'template-plan',
      jobId: 'template-job',
      ordinal: 0,
      kind: 'codex.plan',
      input: { prompt: 'Create the plan.' },
      state: 'succeeded',
      maxAttempts: 3,
      nextEligibleAt: now,
      createdAt: now,
      updatedAt: now,
    },
  ],
};

const definitions = [];
let nextJob = 0;
const coordinator = {
  async loadJob(jobId) {
    assert.equal(jobId, template.job.jobId);
    return template;
  },
  async createJob(definition) {
    definitions.push(definition);
    nextJob += 1;
    const jobId = `scheduled-job-${nextJob}`;
    return {
      job: {
        jobId,
        kind: definition.kind,
        input: definition.input,
        state: 'queued',
        workflowRunId: definition.workflowRunId ?? null,
        createdAt: now,
        updatedAt: now,
      },
      operations: definition.operations.map((operation, ordinal) => ({
        operationId: `${jobId}-operation-${ordinal}`,
        jobId,
        ordinal,
        kind: operation.kind,
        input: operation.input,
        state: 'ready',
        maxAttempts: operation.maxAttempts,
        nextEligibleAt: now,
        createdAt: now,
        updatedAt: now,
      })),
    };
  },
};

const first = await instantiateFactoryJobTemplate(
  { templateJobId: template.job.jobId },
  'cron-workflow-run-1',
  coordinator,
);
const second = await instantiateFactoryJobTemplate(
  { templateJobId: template.job.jobId },
  'cron-workflow-run-2',
  coordinator,
);

assert.notEqual(first.job.jobId, template.job.jobId);
assert.notEqual(second.job.jobId, template.job.jobId);
assert.notEqual(first.job.jobId, second.job.jobId);
assert.equal(definitions.length, 2);
assert.deepEqual(
  definitions.map((definition) => definition.workflowRunId),
  ['cron-workflow-run-1', 'cron-workflow-run-2'],
);
assert.deepEqual(
  definitions[0].operations.map((operation) => ({
    kind: operation.kind,
    maxAttempts: operation.maxAttempts,
  })),
  [
    { kind: 'codex.plan', maxAttempts: 3 },
    { kind: 'codex.execute', maxAttempts: 5 },
  ],
);
assert.deepEqual(definitions[0].input, template.job.input);
assert.notStrictEqual(definitions[0].input, template.job.input);
assert.notStrictEqual(definitions[0].operations[0].input, template.operations[1].input);
assert.deepEqual(template.operations.map((operation) => operation.ordinal), [1, 0]);

console.log('scheduled job template fixture passed');
