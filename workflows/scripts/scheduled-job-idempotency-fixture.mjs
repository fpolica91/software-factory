import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';

import {
  CoordinatorClient,
  CoordinatorHttpError,
} from '../dist/coordinator-client.js';

if (!process.env.FACTORYD_URL) {
  throw new Error('FACTORYD_URL must point at the factoryd instance under test');
}

const coordinator = new CoordinatorClient();
const workflowRunId = `scheduled-job-fixture-${randomUUID()}`;
const definition = {
  kind: 'scheduled-fixture',
  input: {
    cwd: '/tmp/scheduled-fixture',
    prompt: 'Do not execute; this fixture only verifies durable job creation.',
  },
  workflowRunId,
  operations: [
    {
      kind: 'codex.plan',
      input: { prompt: 'Create a plan.' },
      maxAttempts: 3,
    },
    {
      kind: 'codex.execute',
      input: { prompt: 'Execute the plan.' },
      maxAttempts: 5,
    },
  ],
};

const [first, replay] = await Promise.all([
  coordinator.createJob(definition),
  coordinator.createJob(structuredClone(definition)),
]);
assert.equal(replay.job.jobId, first.job.jobId);
assert.deepEqual(replay.operations, first.operations);

const distinct = await coordinator.createJob({
  ...structuredClone(definition),
  workflowRunId: `${workflowRunId}-distinct`,
});
assert.notEqual(distinct.job.jobId, first.job.jobId);

await assert.rejects(
  coordinator.createJob({
    ...structuredClone(definition),
    input: { ...definition.input, prompt: 'A conflicting definition.' },
  }),
  (error) => error instanceof CoordinatorHttpError &&
    error.status === 409 &&
    error.body.includes('workflowRunConflict'),
);

console.log(JSON.stringify({
  fixture: 'scheduled-job-idempotency',
  workflowRunId,
  firstJobId: first.job.jobId,
  replayJobId: replay.job.jobId,
  distinctJobId: distinct.job.jobId,
  conflictRejected: true,
}, null, 2));
