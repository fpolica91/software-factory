export type JsonPrimitive = boolean | number | string | null;
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue };

export interface IntegrationCursor {
  value: string;
}

export interface IntegrationPage<T> {
  items: T[];
  nextCursor?: IntegrationCursor;
}

export interface ExternalReference {
  adapter: string;
  externalId: string;
  url?: string;
}

export interface FactoryExecutionReference {
  jobId: string;
  operationId?: string;
  attemptId?: string;
  threadId?: string;
  turnId?: string;
  workspaceRevision?: string;
}

export interface RepositoryLocator {
  cloneUrl: string;
  baseRef?: string;
}

export interface FactoryIntake {
  reference: ExternalReference;
  title: string;
  prompt: string;
  repository: RepositoryLocator;
  metadata?: Record<string, JsonValue>;
}

export type FactoryLifecycleEvent =
  | {
      eventId: string;
      type: 'job.started' | 'job.completed';
      execution: FactoryExecutionReference;
      detail?: JsonValue;
    }
  | {
      eventId: string;
      type: 'job.failed';
      execution: FactoryExecutionReference;
      error: JsonValue;
    }
  | {
      eventId: string;
      type: 'plan.completed' | 'implementation.completed' | 'review.completed' | 'remediation.completed';
      execution: FactoryExecutionReference;
      result: JsonValue;
    };
