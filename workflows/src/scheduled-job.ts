import { CoordinatorClient } from './coordinator-client.js';
import { executeFactoryJob } from './factory-job.js';
import type {
  DurableJob,
  FactoryJobContext,
  FactoryJobResult,
  JobDefinition,
} from './types.js';

export interface FactoryJobTemplateInput {
  [key: string]: string;
  templateJobId: string;
}

type TemplateCoordinator = Pick<CoordinatorClient, 'createJob' | 'loadJob'>;

export function factoryJobDefinitionFromTemplate(
  template: DurableJob,
  workflowRunId: string,
): JobDefinition {
  return {
    kind: template.job.kind,
    input: structuredClone(template.job.input),
    workflowRunId,
    operations: [...template.operations]
      .sort((left, right) => left.ordinal - right.ordinal)
      .map((operation) => ({
        kind: operation.kind,
        input: structuredClone(operation.input),
        maxAttempts: operation.maxAttempts,
      })),
  };
}

export async function instantiateFactoryJobTemplate(
  input: FactoryJobTemplateInput,
  workflowRunId: string,
  coordinator: TemplateCoordinator = new CoordinatorClient(),
): Promise<DurableJob> {
  if (!input.templateJobId.trim()) {
    throw new Error('templateJobId must be a nonempty string');
  }
  if (!workflowRunId.trim()) {
    throw new Error('workflowRunId must be a nonempty string');
  }
  const template = await coordinator.loadJob(input.templateJobId);
  return coordinator.createJob(factoryJobDefinitionFromTemplate(template, workflowRunId));
}

export async function executeFactoryJobTemplate(
  input: FactoryJobTemplateInput,
  workflowRunId: string,
  context: FactoryJobContext,
  coordinator = new CoordinatorClient(),
): Promise<FactoryJobResult> {
  const fresh = await instantiateFactoryJobTemplate(input, workflowRunId, coordinator);
  await context.log(
    `created scheduled job ${fresh.job.jobId} from template ${input.templateJobId}`,
  );
  return executeFactoryJob({ jobId: fresh.job.jobId }, context, coordinator);
}
