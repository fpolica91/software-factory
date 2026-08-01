import assert from 'node:assert/strict';

import {
  FactoryStateGateError,
  assertFactoryStageState,
} from '../dist/factory-state-gate.js';

const validState = {
  decomposition: {
    revision: 4,
    work_units: [
      {
        id: 'design',
        title: 'Design the change',
        description: 'Define the implementation contract.',
        depends_on: [],
      },
      {
        id: 'implement',
        title: 'Implement the change',
        description: 'Build the behavior defined by the contract.',
        depends_on: ['design'],
      },
    ],
  },
  progress: {
    work_units: [
      {
        id: 'design',
        status: 'completed',
        progress_summary: 'The contract is documented.',
      },
      {
        id: 'implement',
        status: 'completed',
        progress_summary: 'The implementation is complete.',
      },
    ],
  },
  review: {
    generation: 1,
    recorded_turn_id: 'review-turn-1',
    recorded_thread_id: 'review-thread-1',
    recorded_parent_thread_id: 'factory-thread',
    recorded_parent_turn_id: 'factory-review-turn-1',
    recorded_subagent_kind: 'review',
    verdict: 'request_changes',
    summary: 'Two concrete findings require dispositions.',
    findings: [
      {
        id: 'F1',
        severity: 'major',
        unit_id: 'implement',
        title: 'Handle retry state',
        evidence: 'The retry path omitted durable state.',
        recommendation: 'Persist retry state before returning.',
      },
      {
        id: 'F2',
        severity: 'minor',
        unit_id: 'design',
        title: 'Clarify the contract',
        evidence: 'The contract omitted one transition.',
        recommendation: 'Document the missing transition.',
      },
    ],
  },
  remediation: {
    records: [
      {
        finding_id: 'F1',
        disposition: 'resolved',
        rationale: 'Retry state is now persisted.',
        unit_id: 'implement',
      },
      {
        finding_id: 'F2',
        disposition: 'accepted',
        rationale: 'The contract now documents the transition.',
        unit_id: 'design',
      },
    ],
  },
};

function clone(value) {
  return structuredClone(value);
}

function expectGateFailure(stage, state, message) {
  assert.throws(
    () => assertFactoryStageState(stage, state),
    (error) => error instanceof FactoryStateGateError && message.test(error.message),
  );
}

for (const stage of ['codex.plan', 'codex.execute', 'codex.review', 'codex.remediate']) {
  assert.doesNotThrow(() => assertFactoryStageState(stage, validState));
}

const emptyPlan = clone(validState);
emptyPlan.decomposition.work_units = [];
expectGateFailure('codex.plan', emptyPlan, /at least one work unit/);

const cyclicPlan = clone(validState);
cyclicPlan.decomposition.work_units[0].depends_on = ['implement'];
expectGateFailure('codex.plan', cyclicPlan, /contain a cycle/);

const incompleteExecution = clone(validState);
incompleteExecution.progress.work_units[1].status = 'in_progress';
expectGateFailure('codex.execute', incompleteExecution, /not completed: implement/);

const missingReview = clone(validState);
delete missingReview.review;
expectGateFailure('codex.review', missingReview, /review must be an object/);

const incompleteRemediation = clone(validState);
incompleteRemediation.remediation.records.pop();
expectGateFailure('codex.remediate', incompleteRemediation, /missing findings: F2/);

const approvedState = clone(validState);
approvedState.review = {
  generation: 2,
  recorded_turn_id: 'review-turn-2',
  recorded_thread_id: 'review-thread-2',
  recorded_parent_thread_id: 'factory-thread',
  recorded_parent_turn_id: 'factory-review-turn-2',
  recorded_subagent_kind: 'review',
  verdict: 'approve',
  summary: 'The work is approved.',
  findings: [],
};
delete approvedState.remediation;
assert.doesNotThrow(() => assertFactoryStageState('codex.remediate', approvedState));

const legacyReview = clone(validState);
delete legacyReview.review.generation;
delete legacyReview.review.recorded_turn_id;
delete legacyReview.review.recorded_thread_id;
delete legacyReview.review.recorded_parent_thread_id;
delete legacyReview.review.recorded_parent_turn_id;
delete legacyReview.review.recorded_subagent_kind;
assert.doesNotThrow(() => assertFactoryStageState('codex.remediate', legacyReview));
expectGateFailure('codex.review', legacyReview, /review.generation must be a positive integer/);

console.log('factory state gates fixture passed');
