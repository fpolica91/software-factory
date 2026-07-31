import type {
  FactoryCorrelation,
  FactoryEvent,
  FactoryMethod,
  FactoryRequestId,
  FactoryResponseEnvelope,
} from './protocol/v1/generated/index.js';
import type { CodexNotification } from './codex-v2/wire.js';

export type FactoryCorrelationSeed = Pick<
  FactoryCorrelation,
  'jobId' | 'operationId' | 'attemptId'
> & Partial<Pick<FactoryCorrelation, 'workflowRunId' | 'taskRunExternalId'>>;

export interface FactoryCorrelatedResult<T> {
  response: T;
  correlation: FactoryCorrelation;
}

export interface FactoryCodexCorrelatedResult<T, M extends string = string>
  extends FactoryCorrelatedResult<T> {
  /** Exact JSON-RPC request id sent on the Codex V2 wire. */
  requestId: FactoryRequestId;
  method: M;
}

export interface FactoryCorrelatedCodexNotification {
  notification: CodexNotification;
  correlation?: FactoryCorrelation;
}

export interface FactoryCorrelatedEvent {
  event: FactoryEvent;
  correlation?: FactoryCorrelation;
}

function clone(correlation: FactoryCorrelation): FactoryCorrelation {
  return { ...correlation };
}

function stringField(value: unknown, field: string): string | undefined {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
  const fieldValue = (value as Record<string, unknown>)[field];
  return typeof fieldValue === 'string' ? fieldValue : undefined;
}

export class FactoryCorrelationMap {
  readonly #requests = new Map<FactoryRequestId, FactoryCorrelation>();
  readonly #threads = new Map<string, FactoryCorrelation>();
  readonly #threadOrigins = new Map<string, FactoryCorrelation>();
  readonly #turns = new Map<string, FactoryCorrelation>();
  readonly #turnOrigins = new Map<string, FactoryCorrelation>();
  readonly #items = new Map<string, FactoryCorrelation>();

  begin(
    seed: FactoryCorrelationSeed,
    requestId: FactoryRequestId,
    _method: FactoryMethod | string,
    params: unknown,
  ): FactoryCorrelation {
    const correlation: FactoryCorrelation = { ...seed, requestId };
    const threadId = stringField(params, 'threadId');
    const turnId = stringField(params, 'turnId') ?? stringField(params, 'expectedTurnId');
    if (threadId !== undefined) correlation.threadId = threadId;
    if (turnId !== undefined) correlation.turnId = turnId;
    this.#requests.set(requestId, correlation);
    if (threadId !== undefined) this.#threads.set(threadId, correlation);
    if (turnId !== undefined) this.#turns.set(turnId, correlation);
    return clone(correlation);
  }

  complete(requestId: FactoryRequestId, response: FactoryResponseEnvelope): FactoryCorrelation | undefined {
    return this.completeResult(requestId, response.method, response.result);
  }

  completeResult(
    requestId: FactoryRequestId,
    method: FactoryMethod | string,
    result: unknown,
  ): FactoryCorrelation | undefined {
    const correlation = this.#requests.get(requestId);
    if (!correlation) return undefined;
    const threadId = stringField(result, 'threadId') ?? stringField(
      typeof result === 'object' && result !== null ? (result as Record<string, unknown>).thread : undefined,
      'id',
    );
    const turnId = stringField(result, 'turnId') ?? stringField(
      typeof result === 'object' && result !== null ? (result as Record<string, unknown>).turn : undefined,
      'id',
    );
    if (threadId !== undefined) {
      correlation.threadId = threadId;
      this.#threads.set(threadId, correlation);
      if (method === 'thread/start' || method === 'thread/resume' || method === 'thread/fork') {
        this.#threadOrigins.set(threadId, correlation);
      }
    }
    if (turnId !== undefined) {
      correlation.turnId = turnId;
      this.#turns.set(turnId, correlation);
      if (method === 'turn/start' || method === 'review/start') {
        this.#turnOrigins.set(turnId, correlation);
      }
    }
    return clone(correlation);
  }

  observe(event: FactoryEvent): FactoryCorrelation | undefined {
    const eventRecord = event as unknown as Record<string, unknown>;
    const serverParams = event.type === 'serverRequest'
      ? event.request.request.params
      : undefined;
    const threadId = stringField(eventRecord, 'threadId') ??
      stringField(eventRecord.thread, 'id') ??
      stringField(serverParams, 'threadId');
    const turnId = stringField(eventRecord, 'turnId') ??
      stringField(eventRecord.turn, 'id') ??
      stringField(serverParams, 'turnId');
    const itemId = stringField(eventRecord, 'itemId') ??
      stringField(eventRecord.item, 'id') ??
      stringField(serverParams, 'itemId');
    return this.#observeIdentity({
      threadId,
      turnId,
      itemId,
      threadStarted: event.type === 'threadStarted',
      turnStarted: event.type === 'turnStarted',
    });
  }

  observeCodexNotification(notification: CodexNotification): FactoryCorrelation | undefined {
    const exact = notification.notification;
    const params = exact.params as unknown;
    const threadId = stringField(params, 'threadId') ?? stringField(
      typeof params === 'object' && params !== null
        ? (params as Record<string, unknown>).thread
        : undefined,
      'id',
    );
    const turnId = stringField(params, 'turnId') ?? stringField(
      typeof params === 'object' && params !== null
        ? (params as Record<string, unknown>).turn
        : undefined,
      'id',
    );
    const itemId = stringField(params, 'itemId') ?? stringField(
      typeof params === 'object' && params !== null
        ? (params as Record<string, unknown>).item
        : undefined,
      'id',
    );
    return this.#observeIdentity({
      threadId,
      turnId,
      itemId,
      threadStarted: exact.method === 'thread/started',
      turnStarted: exact.method === 'turn/started',
    });
  }

  #observeIdentity(identity: {
    threadId: string | undefined;
    turnId: string | undefined;
    itemId: string | undefined;
    threadStarted: boolean;
    turnStarted: boolean;
  }): FactoryCorrelation | undefined {
    const { threadId, turnId, itemId, threadStarted, turnStarted } = identity;
    if (threadStarted && threadId) {
      const origin = this.#threadOrigins.get(threadId);
      if (origin) return { ...origin, threadId };
    }
    if (turnStarted && turnId) {
      const origin = this.#turnOrigins.get(turnId);
      if (origin) return { ...origin, ...(threadId ? { threadId } : {}), turnId };
    }
    const base = (itemId && this.#items.get(itemId)) ||
      (turnId && this.#turns.get(turnId)) ||
      (threadId && this.#threads.get(threadId));
    if (!base) return undefined;
    const correlation = clone(base);
    if (threadId !== undefined) {
      correlation.threadId = threadId;
      this.#threads.set(threadId, correlation);
    }
    if (turnId !== undefined) {
      correlation.turnId = turnId;
      this.#turns.set(turnId, correlation);
    }
    if (itemId !== undefined) {
      correlation.itemId = itemId;
      this.#items.set(itemId, correlation);
    }
    return correlation;
  }

  forRequest(requestId: FactoryRequestId): FactoryCorrelation | undefined {
    const value = this.#requests.get(requestId);
    return value && clone(value);
  }

  forThread(threadId: string): FactoryCorrelation | undefined {
    const value = this.#threads.get(threadId);
    return value && clone(value);
  }

  forTurn(turnId: string): FactoryCorrelation | undefined {
    const value = this.#turns.get(turnId);
    return value && clone(value);
  }

  forItem(itemId: string): FactoryCorrelation | undefined {
    const value = this.#items.get(itemId);
    return value && clone(value);
  }
}
