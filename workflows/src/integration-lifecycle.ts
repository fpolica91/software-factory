import {
  IntegrationRegistry,
  loadIntegrations,
  type ExternalReference,
  type FactoryExecutionReference,
  type FactoryLifecycleEvent,
  type IntegrationPluginConfig,
  type JsonValue as IntegrationJsonValue,
} from '@software-factory/integrations';
import type { JsonValue } from '@software-factory/harness-client';
import type { OperationKind, OperationResult, OperationRecord } from './types.js';

const PLUGINS_ENV = 'FACTORY_INTEGRATION_PLUGINS_JSON';

let registryConfiguration: string | undefined;
let registryPromise: Promise<IntegrationRegistry> | undefined;

function object(value: JsonValue | undefined, label: string): Record<string, JsonValue> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value as Record<string, JsonValue>;
}

function requiredString(value: JsonValue | undefined, label: string): string {
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(`${label} must be a non-empty string`);
  }
  return value;
}

function optionalString(value: JsonValue | undefined, label: string): string | undefined {
  if (value === undefined) return undefined;
  return requiredString(value, label);
}

function pluginConfiguration(raw: string): IntegrationPluginConfig[] {
  const parsed = JSON.parse(raw) as unknown;
  if (!Array.isArray(parsed)) {
    throw new Error(`${PLUGINS_ENV} must contain a JSON array`);
  }
  return parsed.map((value, index) => {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) {
      throw new Error(`${PLUGINS_ENV}[${index}] must be an object`);
    }
    const entry = value as Record<string, unknown>;
    if (typeof entry.module !== 'string' || entry.module.trim() === '') {
      throw new Error(`${PLUGINS_ENV}[${index}].module must be a non-empty string`);
    }
    if (entry.config !== undefined &&
      (typeof entry.config !== 'object' || entry.config === null || Array.isArray(entry.config))) {
      throw new Error(`${PLUGINS_ENV}[${index}].config must be an object`);
    }
    return {
      module: entry.module,
      ...(entry.config
        ? { config: entry.config as Record<string, IntegrationJsonValue> }
        : {}),
    };
  });
}

async function integrationRegistry(): Promise<IntegrationRegistry> {
  const configuration = process.env[PLUGINS_ENV];
  if (!configuration) {
    throw new Error(
      `job input selects an integration, but ${PLUGINS_ENV} is not configured`,
    );
  }
  if (!registryPromise || registryConfiguration !== configuration) {
    registryConfiguration = configuration;
    registryPromise = (async () => {
      const registry = new IntegrationRegistry();
      await loadIntegrations(registry, pluginConfiguration(configuration));
      return registry;
    })();
  }
  return registryPromise;
}

function externalReference(jobInput: Record<string, JsonValue>): ExternalReference | undefined {
  const integrationValue = jobInput.integration;
  if (integrationValue === undefined) return undefined;
  const integration = object(integrationValue, 'job input integration');
  const intake = object(integration.intake, 'job input integration.intake');
  const url = optionalString(intake.url, 'job input integration.intake.url');
  return {
    adapter: requiredString(intake.adapter, 'job input integration.intake.adapter'),
    externalId: requiredString(
      intake.externalId,
      'job input integration.intake.externalId',
    ),
    ...(url ? { url } : {}),
  };
}

function completionEventType(kind: OperationKind): Extract<
  FactoryLifecycleEvent,
  { type: `${string}.completed` }
>['type'] {
  switch (kind) {
    case 'codex.plan': return 'plan.completed';
    case 'codex.execute': return 'implementation.completed';
    case 'codex.review': return 'review.completed';
    case 'codex.remediate': return 'remediation.completed';
  }
}

/**
 * Publishes external lifecycle state inside the same retry boundary as a
 * factoryd attempt. Adapters must deduplicate by eventId because Hatchet may
 * replay a successfully delivered event after a worker crash.
 */
export class IntegrationLifecyclePublisher {
  private constructor(
    readonly reference: ExternalReference,
    readonly publish: (event: FactoryLifecycleEvent) => Promise<void>,
  ) {}

  static async fromJobInput(
    jobInput: Record<string, JsonValue>,
  ): Promise<IntegrationLifecyclePublisher | undefined> {
    const reference = externalReference(jobInput);
    if (!reference) return undefined;
    const registry = await integrationRegistry();
    const adapter = registry.intake(reference.adapter);
    return new IntegrationLifecyclePublisher(
      reference,
      (event) => adapter.publishLifecycle(reference, event),
    );
  }

  async jobStarted(jobId: string, operation: OperationRecord): Promise<void> {
    await this.publish({
      eventId: `factory:${jobId}:job.started`,
      type: 'job.started',
      execution: {
        jobId,
        operationId: operation.operationId,
      },
    });
  }

  async stageCompleted(options: {
    jobId: string;
    attemptId: string;
    operation: OperationRecord;
    kind: OperationKind;
    result: OperationResult;
    recoveredFromCheckpoint: boolean;
  }): Promise<void> {
    const { jobId, attemptId, operation, kind, result, recoveredFromCheckpoint } = options;
    const execution: FactoryExecutionReference = {
      jobId,
      operationId: operation.operationId,
      attemptId,
      threadId: result.threadId,
      ...(result.turnId ? { turnId: result.turnId } : {}),
      ...(result.workspaceRevision
        ? { workspaceRevision: result.workspaceRevision }
        : {}),
    };
    await this.publish({
      eventId: `factory:${jobId}:${operation.operationId}:${kind}.completed`,
      type: completionEventType(kind),
      execution,
      result: {
        recoveredFromCheckpoint,
        ...(result.turn ? { turn: result.turn as unknown as IntegrationJsonValue } : {}),
      },
    });
  }

  async jobCompleted(jobId: string, result: OperationResult): Promise<void> {
    await this.publish({
      eventId: `factory:${jobId}:job.completed`,
      type: 'job.completed',
      execution: {
        jobId,
        threadId: result.threadId,
        ...(result.turnId ? { turnId: result.turnId } : {}),
        ...(result.workspaceRevision
          ? { workspaceRevision: result.workspaceRevision }
          : {}),
      },
    });
  }
}
