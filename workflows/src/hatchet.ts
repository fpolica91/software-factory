import { hostname } from 'node:os';
import {
  ConcurrencyLimitStrategy,
  HatchetClient,
  NonRetryableError,
} from '@hatchet-dev/typescript-sdk/v1/index.js';
import { CoordinatorClient } from './coordinator-client.js';
import { executeFactoryJob } from './factory-job.js';
import { TerminalOperationError } from './lifecycle.js';
import {
  executeFactoryJobTemplate,
  type FactoryJobTemplateInput,
} from './scheduled-job.js';
import type { FactoryJobInput, FactoryJobResult } from './types.js';

export const hatchet = HatchetClient.init();

function configuredTaskRetries(): number {
  const retries = Number(process.env.FACTORY_HATCHET_TASK_RETRIES ?? 20);
  if (!Number.isSafeInteger(retries) || retries < 1) {
    throw new Error('FACTORY_HATCHET_TASK_RETRIES must be a positive integer');
  }
  return retries;
}

const taskRetries = configuredTaskRetries();

async function runWithoutTerminalRetry<T>(action: () => Promise<T>): Promise<T> {
  try {
    return await action();
  } catch (error) {
    if (error instanceof TerminalOperationError) {
      throw new NonRetryableError(error.message);
    }
    throw error;
  }
}

export const factoryJob = hatchet.durableTask<FactoryJobInput, FactoryJobResult>({
  name: 'factory-job',
  executionTimeout: '168h',
  scheduleTimeout: '168h',
  retries: taskRetries,
  concurrency: {
    expression: 'input.jobId',
    maxRuns: 1,
    limitStrategy: ConcurrencyLimitStrategy.CANCEL_NEWEST,
  },
  fn: async (input, ctx) => runWithoutTerminalRetry(
    () => executeFactoryJob(input, {
        ownerInstanceId: process.env.FACTORY_WORKER_INSTANCE_ID ?? `${hostname()}-${process.pid}`,
        workflowRunId: ctx.workflowRunId(),
        taskRunExternalId: ctx.taskRunExternalId(),
        log: async (message) => {
          await ctx.log(message);
        },
        sleepUntil: async (wakeAt) => {
          await ctx.sleepUntil(wakeAt);
        },
      }, new CoordinatorClient()),
  ),
});

export const factoryJobFromTemplate = hatchet.durableTask<
  FactoryJobTemplateInput,
  FactoryJobResult
>({
  name: 'factory-job-from-template',
  executionTimeout: '168h',
  scheduleTimeout: '168h',
  retries: taskRetries,
  concurrency: {
    expression: 'input.templateJobId',
    maxRuns: 1,
    limitStrategy: ConcurrencyLimitStrategy.CANCEL_NEWEST,
  },
  fn: async (input, ctx) => runWithoutTerminalRetry(
    () => executeFactoryJobTemplate(
      input,
      ctx.workflowRunId(),
      {
        ownerInstanceId: process.env.FACTORY_WORKER_INSTANCE_ID ?? `${hostname()}-${process.pid}`,
        workflowRunId: ctx.workflowRunId(),
        taskRunExternalId: ctx.taskRunExternalId(),
        log: async (message) => {
          await ctx.log(message);
        },
        sleepUntil: async (wakeAt) => {
          await ctx.sleepUntil(wakeAt);
        },
      },
      new CoordinatorClient(),
    ),
  ),
});
