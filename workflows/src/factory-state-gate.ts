import type { JsonValue } from '@software-factory/harness-client';
import type { FactoryThreadStateDocument, OperationKind } from './types.js';

export const FACTORY_STAGE_CONTRACT: Record<OperationKind, string> = {
  'codex.plan': [
    'This Factory plan stage is incomplete until durable decomposition is recorded.',
    'Before the final response, call factory_decompose with a nonempty valid DAG of actionable work units.',
  ].join(' '),
  'codex.execute': [
    'Use factory_update_progress while executing the durable decomposition.',
    'Do not finish this stage until every current Factory work unit has status completed.',
  ].join(' '),
  'codex.review': [
    'Before the final response, call factory_record_review with the current structured verdict, summary, and findings.',
    'Every finding must reference an existing Factory work unit.',
  ].join(' '),
  'codex.remediate': [
    'Inspect the current durable Factory review before finishing this stage.',
    'When its verdict is not approve, call factory_record_remediation with exactly one disposition for every current finding; an approved review needs no remediation mutation.',
  ].join(' '),
};

const PROGRESS_STATUSES = ['pending', 'in_progress', 'completed', 'blocked'] as const;
const REVIEW_VERDICTS = ['approve', 'request_changes', 'blocked'] as const;
const FINDING_SEVERITIES = ['critical', 'major', 'minor'] as const;
const REMEDIATION_DISPOSITIONS = ['accepted', 'rejected', 'deferred', 'resolved'] as const;

interface FactoryWorkUnitDefinition {
  id: string;
  dependencies: string[];
}

interface FactoryReviewState {
  verdict: (typeof REVIEW_VERDICTS)[number];
  findings: Map<string, { unitId: string }>;
}

export class FactoryStateGateError extends Error {
  constructor(
    readonly stage: OperationKind,
    detail: string,
  ) {
    super(`${stage} durable Factory state gate failed: ${detail}`);
    this.name = 'FactoryStateGateError';
  }
}

function gateError(stage: OperationKind, detail: string): never {
  throw new FactoryStateGateError(stage, detail);
}

function stateObject(
  stage: OperationKind,
  value: JsonValue | undefined,
  label: string,
): Record<string, JsonValue> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return gateError(stage, `${label} must be an object`);
  }
  return value as Record<string, JsonValue>;
}

function stateArray(
  stage: OperationKind,
  value: JsonValue | undefined,
  label: string,
): JsonValue[] {
  if (!Array.isArray(value)) return gateError(stage, `${label} must be an array`);
  return value;
}

function stateText(
  stage: OperationKind,
  value: JsonValue | undefined,
  label: string,
): string {
  if (typeof value !== 'string' || value.trim() === '') {
    return gateError(stage, `${label} must be a nonempty string`);
  }
  return value;
}

function stateEnum<T extends string>(
  stage: OperationKind,
  value: JsonValue | undefined,
  label: string,
  allowed: readonly T[],
): T {
  if (typeof value !== 'string' || !(allowed as readonly string[]).includes(value)) {
    return gateError(stage, `${label} must be one of ${allowed.join(', ')}`);
  }
  return value as T;
}

function decomposition(
  stage: OperationKind,
  state: FactoryThreadStateDocument,
): Map<string, FactoryWorkUnitDefinition> {
  const document = stateObject(stage, state.decomposition, 'decomposition');
  if (
    typeof document.revision !== 'number' ||
    !Number.isSafeInteger(document.revision) ||
    document.revision < 1
  ) {
    gateError(stage, 'decomposition.revision must be a positive integer');
  }
  const values = stateArray(stage, document.work_units, 'decomposition.work_units');
  if (values.length === 0) gateError(stage, 'decomposition requires at least one work unit');

  const units = new Map<string, FactoryWorkUnitDefinition>();
  for (const [index, value] of values.entries()) {
    const unit = stateObject(stage, value, `decomposition.work_units[${index}]`);
    const id = stateText(stage, unit.id, `decomposition.work_units[${index}].id`);
    stateText(stage, unit.title, `decomposition.work_units[${index}].title`);
    stateText(stage, unit.description, `decomposition.work_units[${index}].description`);
    if (units.has(id)) gateError(stage, `decomposition repeats work unit ${id}`);
    const dependencies = stateArray(
      stage,
      unit.depends_on,
      `decomposition.work_units[${index}].depends_on`,
    ).map((dependency, dependencyIndex) => stateText(
      stage,
      dependency,
      `decomposition.work_units[${index}].depends_on[${dependencyIndex}]`,
    ));
    if (new Set(dependencies).size !== dependencies.length) {
      gateError(stage, `work unit ${id} repeats a dependency`);
    }
    units.set(id, { id, dependencies });
  }

  for (const unit of units.values()) {
    for (const dependency of unit.dependencies) {
      if (dependency === unit.id) gateError(stage, `work unit ${unit.id} depends on itself`);
      if (!units.has(dependency)) {
        gateError(stage, `work unit ${unit.id} depends on unknown unit ${dependency}`);
      }
    }
  }

  const remaining = new Map(
    [...units.values()].map((unit) => [unit.id, unit.dependencies.length]),
  );
  const completed = new Set<string>();
  while (true) {
    const ready = [...remaining]
      .filter(([id, dependencyCount]) => !completed.has(id) && dependencyCount === 0)
      .map(([id]) => id);
    if (ready.length === 0) break;
    for (const id of ready) {
      completed.add(id);
      for (const unit of units.values()) {
        if (!completed.has(unit.id) && unit.dependencies.includes(id)) {
          remaining.set(unit.id, Math.max(0, (remaining.get(unit.id) ?? 0) - 1));
        }
      }
    }
  }
  if (completed.size !== units.size) {
    gateError(stage, 'work-unit dependencies contain a cycle');
  }
  return units;
}

function requireCompletedProgress(
  stage: OperationKind,
  state: FactoryThreadStateDocument,
): void {
  const units = decomposition(stage, state);
  const document = stateObject(stage, state.progress, 'progress');
  const values = stateArray(stage, document.work_units, 'progress.work_units');
  const seen = new Set<string>();
  const incomplete: string[] = [];
  for (const [index, value] of values.entries()) {
    const progress = stateObject(stage, value, `progress.work_units[${index}]`);
    const id = stateText(stage, progress.id, `progress.work_units[${index}].id`);
    if (!units.has(id)) gateError(stage, `progress references unknown work unit ${id}`);
    if (!seen.add(id)) gateError(stage, `progress repeats work unit ${id}`);
    const status = stateEnum(
      stage,
      progress.status,
      `progress for ${id}`,
      PROGRESS_STATUSES,
    );
    if (status !== 'completed') incomplete.push(id);
    if (status === 'completed') {
      stateText(stage, progress.progress_summary, `completed progress summary for ${id}`);
    }
  }
  const missing = [...units.keys()].filter((id) => !seen.has(id));
  if (missing.length > 0) gateError(stage, `progress is missing work units: ${missing.join(', ')}`);
  if (incomplete.length > 0) {
    gateError(stage, `work units are not completed: ${incomplete.join(', ')}`);
  }
}

function reviewState(
  stage: OperationKind,
  state: FactoryThreadStateDocument,
): FactoryReviewState {
  const units = decomposition(stage, state);
  const review = stateObject(stage, state.review, 'review');
  const verdict = stateEnum(stage, review.verdict, 'review.verdict', REVIEW_VERDICTS);
  stateText(stage, review.summary, 'review.summary');
  const values = stateArray(stage, review.findings, 'review.findings');
  if (verdict !== 'approve' && values.length === 0) {
    gateError(stage, 'a non-approved review requires at least one finding');
  }
  const findings = new Map<string, { unitId: string }>();
  for (const [index, value] of values.entries()) {
    const finding = stateObject(stage, value, `review.findings[${index}]`);
    const id = stateText(stage, finding.id, `review.findings[${index}].id`);
    if (findings.has(id)) gateError(stage, `review repeats finding ${id}`);
    stateEnum(stage, finding.severity, `review finding ${id} severity`, FINDING_SEVERITIES);
    const unitId = stateText(stage, finding.unit_id, `review finding ${id} unit_id`);
    if (!units.has(unitId)) gateError(stage, `review finding ${id} references unknown unit ${unitId}`);
    stateText(stage, finding.title, `review finding ${id} title`);
    stateText(stage, finding.evidence, `review finding ${id} evidence`);
    stateText(stage, finding.recommendation, `review finding ${id} recommendation`);
    findings.set(id, { unitId });
  }
  return { verdict, findings };
}

function requireCompleteRemediation(
  stage: OperationKind,
  state: FactoryThreadStateDocument,
): void {
  const review = reviewState(stage, state);
  if (review.verdict === 'approve') return;
  const remediation = stateObject(stage, state.remediation, 'remediation');
  const values = stateArray(stage, remediation.records, 'remediation.records');
  const seen = new Set<string>();
  for (const [index, value] of values.entries()) {
    const record = stateObject(stage, value, `remediation.records[${index}]`);
    const findingId = stateText(
      stage,
      record.finding_id,
      `remediation.records[${index}].finding_id`,
    );
    const finding = review.findings.get(findingId);
    if (!finding) gateError(stage, `remediation references unknown finding ${findingId}`);
    if (!seen.add(findingId)) gateError(stage, `remediation repeats finding ${findingId}`);
    const unitId = stateText(stage, record.unit_id, `remediation for ${findingId} unit_id`);
    if (unitId !== finding.unitId) {
      gateError(stage, `remediation for ${findingId} must use work unit ${finding.unitId}`);
    }
    stateEnum(
      stage,
      record.disposition,
      `remediation for ${findingId} disposition`,
      REMEDIATION_DISPOSITIONS,
    );
    stateText(stage, record.rationale, `remediation for ${findingId} rationale`);
  }
  const missing = [...review.findings.keys()].filter((findingId) => !seen.has(findingId));
  if (missing.length > 0) gateError(stage, `remediation is missing findings: ${missing.join(', ')}`);
}

export function assertFactoryStageState(
  stage: OperationKind,
  state: FactoryThreadStateDocument,
): void {
  switch (stage) {
    case 'codex.plan':
      decomposition(stage, state);
      return;
    case 'codex.execute':
      requireCompletedProgress(stage, state);
      return;
    case 'codex.review':
      reviewState(stage, state);
      return;
    case 'codex.remediate':
      requireCompleteRemediation(stage, state);
  }
}
