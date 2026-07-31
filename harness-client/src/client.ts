import type {
  FactoryClientInfo,
  FactoryCorrelation,
  FactoryDecodedServerRequest,
  FactoryEvent,
  FactoryInitializeCapabilities,
  FactoryMethod,
  FactoryMethodNotSupportedResolution,
  FactoryPendingRequest,
  FactoryRequestEnvelope,
  FactoryResponseEnvelope,
  FactoryRpcRequestId,
  FactoryServerErrorResponse,
  FactoryServerResponse,
  InitializeResponse,
  JsonValue,
  ThreadCompactRequest,
  ThreadCompactResponse,
  ThreadForkRequest,
  ThreadForkResponse,
  ThreadResumeRequest,
  ThreadResumeResponse,
  ThreadStartRequest,
  ThreadStartResponse,
  TurnInterruptRequest,
  TurnInterruptResponse,
  TurnStartRequest,
  TurnStartResponse,
  TurnSteerRequest,
  TurnSteerResponse,
} from './protocol/v1/generated/index.js';
import {
  classifyInbound,
  decodeClientResponse,
  decodeNotification,
  decodeServerRequest,
  encodeClientRequest,
  encodeInitializedNotification,
  encodeServerResponse,
  methodNotSupported,
} from './protocol/v1/codec.js';
import {
  CodexRemoteError,
  FactoryClientError,
  FactoryProcessError,
  FactoryRemoteError,
} from './errors.js';
import type { FactoryRuntimeManifest } from './distribution-manifest.js';
import type {
  CodexClientMethod,
  CodexClientRequestArguments,
  CodexClientRequestParams,
  CodexClientResult,
  CodexNotification,
  CodexResponseOutcome,
  CodexServerRequestEvent,
  CodexServerRequestResponse,
} from './codex-v2/wire.js';
import {
  codexNotification,
  codexResponse,
  codexServerRequest,
  codexServerResponse,
} from './codex-v2/wire.js';
import {
  FactoryCorrelationMap,
  type FactoryCorrelatedCodexNotification,
  type FactoryCorrelatedEvent,
  type FactoryCodexCorrelatedResult,
  type FactoryCorrelatedResult,
  type FactoryCorrelationSeed,
} from './correlation.js';
import {
  JsonlProcessTransport,
  negotiateProtocolManifest,
  type FactoryProcessOptions,
} from './transport.js';

export type FactoryServerRequestHandler = (
  request: Extract<FactoryDecodedServerRequest, { kind: 'known' }>,
) => Promise<FactoryServerResponse | FactoryServerErrorResponse>;

export type CodexNotificationHandler = (
  notification: CodexNotification,
) => void | Promise<void>;

export type CodexCorrelatedNotificationHandler = (
  notification: FactoryCorrelatedCodexNotification,
) => void | Promise<void>;

export type CodexServerRequestHandler = (
  request: Extract<CodexServerRequestEvent, { kind: 'known' }>,
) => Promise<CodexServerRequestResponse> | undefined;

export type FactoryClientSignal = {
  type: 'terminateOperation';
  resolution: FactoryMethodNotSupportedResolution;
  correlation?: FactoryCorrelation;
};

export interface FactoryClientOptions extends FactoryProcessOptions {
  clientInfo?: FactoryClientInfo;
  capabilities?: FactoryInitializeCapabilities;
  onEvent?: (event: FactoryCorrelatedEvent) => void | Promise<void>;
  onServerRequest?: FactoryServerRequestHandler;
  onCodexNotification?: CodexNotificationHandler;
  onCodexCorrelatedNotification?: CodexCorrelatedNotificationHandler;
  onCodexServerRequest?: CodexServerRequestHandler;
  onSignal?: (signal: FactoryClientSignal) => void | Promise<void>;
  onError?: (error: Error) => void;
}

interface FactoryPendingRequestState {
  kind: 'factory';
  pending: FactoryPendingRequest;
  resolve(outcome: ReturnType<typeof decodeClientResponse>): void;
  reject(error: Error): void;
}

interface CodexPendingRequestState {
  kind: 'codex';
  id: string;
  method: string;
  resolve(outcome: CodexResponseOutcome): void;
  reject(error: Error): void;
}

type PendingRequest = FactoryPendingRequestState | CodexPendingRequestState;

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T | PromiseLike<T>): void;
  reject(reason: unknown): void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: Deferred<T>['resolve'];
  let reject!: Deferred<T>['reject'];
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

class AsyncQueue<T> implements AsyncIterable<T> {
  readonly #values: T[] = [];
  readonly #waiters: Array<Deferred<IteratorResult<T>>> = [];
  #ended = false;
  #error: Error | undefined;

  push(value: T): void {
    if (this.#ended) return;
    const waiter = this.#waiters.shift();
    if (waiter) waiter.resolve({ done: false, value });
    else this.#values.push(value);
  }

  end(): void {
    if (this.#ended) return;
    this.#ended = true;
    for (const waiter of this.#waiters.splice(0)) waiter.resolve({ done: true, value: undefined });
  }

  fail(error: Error): void {
    if (this.#ended) return;
    this.#ended = true;
    this.#error = error;
    for (const waiter of this.#waiters.splice(0)) waiter.reject(error);
  }

  [Symbol.asyncIterator](): AsyncIterator<T> {
    return {
      next: async (): Promise<IteratorResult<T>> => {
        const value = this.#values.shift();
        if (value !== undefined) return { done: false, value };
        if (this.#error) throw this.#error;
        if (this.#ended) return { done: true, value: undefined };
        const waiter = deferred<IteratorResult<T>>();
        this.#waiters.push(waiter);
        return waiter.promise;
      },
    };
  }
}

export class FactoryClient {
  readonly manifest: FactoryRuntimeManifest;
  readonly events: AsyncIterable<FactoryCorrelatedEvent>;
  readonly codexNotifications: AsyncIterable<CodexNotification>;
  readonly codexCorrelatedNotifications: AsyncIterable<FactoryCorrelatedCodexNotification>;
  readonly codexServerRequests: AsyncIterable<CodexServerRequestEvent>;
  readonly closed: Promise<void>;
  readonly correlations = new FactoryCorrelationMap();

  readonly #transport: JsonlProcessTransport;
  readonly #options: FactoryClientOptions;
  readonly #eventQueue = new AsyncQueue<FactoryCorrelatedEvent>();
  readonly #codexNotificationQueue = new AsyncQueue<CodexNotification>();
  readonly #codexCorrelatedNotificationQueue =
    new AsyncQueue<FactoryCorrelatedCodexNotification>();
  readonly #codexServerRequestQueue = new AsyncQueue<CodexServerRequestEvent>();
  readonly #inflightCodexServerRequests = new Set<Promise<void>>();
  readonly #pending = new Map<string, PendingRequest>();
  readonly #closedDeferred = deferred<void>();
  #nextRequestId = 1;
  #closing = false;
  #settled = false;
  #terminalError: Error | undefined;
  #initializeResponse: InitializeResponse | undefined;

  private constructor(
    options: FactoryClientOptions,
    manifest: FactoryRuntimeManifest,
    transport: JsonlProcessTransport,
  ) {
    this.#options = options;
    this.manifest = manifest;
    this.#transport = transport;
    this.events = this.#eventQueue;
    this.codexNotifications = this.#codexNotificationQueue;
    this.codexCorrelatedNotifications = this.#codexCorrelatedNotificationQueue;
    this.codexServerRequests = this.#codexServerRequestQueue;
    this.closed = this.#closedDeferred.promise;
    void this.closed.catch(() => undefined);
  }

  get initializeResponse(): InitializeResponse {
    if (!this.#initializeResponse) {
      throw new FactoryClientError('Factory client initialization has not completed');
    }
    return this.#initializeResponse;
  }

  static async connect(options: FactoryClientOptions): Promise<FactoryClient> {
    const manifest = await negotiateProtocolManifest(options);
    let client: FactoryClient | undefined;
    const transport = JsonlProcessTransport.start(options, {
      onMessage: async (message) => {
        if (!client) throw new FactoryClientError('factory-runtime emitted a message before initialization');
        await client.#handleMessage(message);
      },
      onError: (error) => {
        if (client) client.#fail(error);
      },
      onExit: (code, signal, stderr) => {
        if (client) client.#handleExit(code, signal, stderr);
      },
    });

    client = new FactoryClient(options, manifest, transport);
    try {
      client.#initializeResponse = await client.#requestWithoutCorrelation<InitializeResponse>('initialize', {
        clientInfo: options.clientInfo ?? {
          name: 'software-factory-harness-client',
          title: 'Software Factory Harness Client',
          version: '0.1.0',
        },
        capabilities: options.capabilities ?? {
          experimentalApi: true,
          requestAttestation: false,
          mcpServerOpenaiFormElicitation: true,
          optOutNotificationMethods: [],
        },
      });
      await transport.send(encodeInitializedNotification());
      return client;
    } catch (error) {
      client.#fail(asError(error));
      await transport.closeInput().catch(() => undefined);
      throw error;
    }
  }

  startThread(
    params: ThreadStartRequest,
    correlation: FactoryCorrelationSeed,
  ): Promise<FactoryCorrelatedResult<ThreadStartResponse>> {
    return this.#request('thread/start', params, correlation);
  }

  resumeThread(
    params: ThreadResumeRequest,
    correlation: FactoryCorrelationSeed,
  ): Promise<FactoryCorrelatedResult<ThreadResumeResponse>> {
    return this.#request('thread/resume', params, correlation);
  }

  forkThread(
    params: ThreadForkRequest,
    correlation: FactoryCorrelationSeed,
  ): Promise<FactoryCorrelatedResult<ThreadForkResponse>> {
    return this.#request('thread/fork', params, correlation);
  }

  compactThread(
    params: ThreadCompactRequest,
    correlation: FactoryCorrelationSeed,
  ): Promise<FactoryCorrelatedResult<ThreadCompactResponse>> {
    return this.#request('thread/compact/start', params, correlation);
  }

  startTurn(
    params: TurnStartRequest,
    correlation: FactoryCorrelationSeed,
  ): Promise<FactoryCorrelatedResult<TurnStartResponse>> {
    return this.#request('turn/start', params, correlation);
  }

  steerTurn(
    params: TurnSteerRequest,
    correlation: FactoryCorrelationSeed,
  ): Promise<FactoryCorrelatedResult<TurnSteerResponse>> {
    return this.#request('turn/steer', params, correlation);
  }

  interruptTurn(
    params: TurnInterruptRequest,
    correlation: FactoryCorrelationSeed,
  ): Promise<FactoryCorrelatedResult<TurnInterruptResponse>> {
    return this.#request('turn/interrupt', params, correlation);
  }

  requestCodex<M extends CodexClientMethod>(
    method: M,
    ...args: CodexClientRequestArguments<M>
  ): Promise<JsonValue> {
    return this.#sendCodex(method, args[0] as JsonValue | undefined);
  }

  requestCodexCorrelated<M extends CodexClientMethod>(
    method: M,
    params: CodexClientRequestParams<M>,
    correlation: FactoryCorrelationSeed,
  ): Promise<FactoryCodexCorrelatedResult<CodexClientResult<M>, M>> {
    return this.#sendCodexCorrelated<CodexClientResult<M>, M>(
      method,
      params as JsonValue,
      correlation,
    );
  }

  requestRaw<TResult = JsonValue>(method: string, params?: JsonValue): Promise<TResult> {
    return this.#sendCodex(method, params) as Promise<TResult>;
  }

  requestRawCorrelated<TResult = JsonValue>(
    method: string,
    params: JsonValue | undefined,
    correlation: FactoryCorrelationSeed,
  ): Promise<FactoryCodexCorrelatedResult<TResult>> {
    return this.#sendCodexCorrelated<TResult, string>(method, params, correlation);
  }

  async close(): Promise<void> {
    if (!this.#closing) {
      this.#closing = true;
      await this.#transport.closeInput();
    }
    await this.closed;
  }

  async #request<T>(
    method: FactoryMethod,
    params: JsonValue | object,
    seed: FactoryCorrelationSeed,
  ): Promise<FactoryCorrelatedResult<T>> {
    const { response, correlation } = await this.#send<T>(method, params, seed);
    if (!correlation) throw new FactoryClientError(`missing durable correlation for ${method}`);
    return { response, correlation };
  }

  async #requestWithoutCorrelation<T>(method: FactoryMethod, params: JsonValue | object): Promise<T> {
    return (await this.#send<T>(method, params)).response;
  }

  async #send<T>(
    method: FactoryMethod,
    params: JsonValue | object,
    seed?: FactoryCorrelationSeed,
  ): Promise<{ response: T; correlation?: ReturnType<FactoryCorrelationMap['forRequest']> }> {
    if (this.#closing || this.#terminalError) {
      throw this.#terminalError ?? new FactoryClientError('Factory client is closing');
    }
    const id = `factory-${this.#nextRequestId++}`;
    const envelope = { id, method, params } as FactoryRequestEnvelope;
    const pending: FactoryPendingRequest = { id, method };
    if (seed) this.correlations.begin(seed, id, method, params);
    const outcome = deferred<ReturnType<typeof decodeClientResponse>>();
    this.#pending.set(id, {
      kind: 'factory',
      pending,
      resolve: outcome.resolve,
      reject: outcome.reject,
    });
    try {
      await this.#transport.send(encodeClientRequest(envelope));
    } catch (error) {
      this.#pending.delete(id);
      throw error;
    }
    const decoded = await outcome.promise;
    if (decoded.type === 'error') throw new FactoryRemoteError(decoded.error);
    const correlation = this.correlations.complete(id, decoded.response);
    return {
      response: decoded.response.result as T,
      ...(correlation ? { correlation } : {}),
    };
  }

  async #sendCodex(method: string, params?: JsonValue): Promise<JsonValue> {
    return (await this.#sendCodexRequest<JsonValue, string>(method, params)).response;
  }

  async #sendCodexCorrelated<T, M extends string>(
    method: M,
    params: JsonValue | undefined,
    seed: FactoryCorrelationSeed,
  ): Promise<FactoryCodexCorrelatedResult<T, M>> {
    const result = await this.#sendCodexRequest<T, M>(method, params, seed);
    if (!result.correlation) {
      throw new FactoryClientError(`missing durable correlation for exact Codex method ${method}`);
    }
    return {
      response: result.response,
      correlation: result.correlation,
      requestId: result.requestId,
      method,
    };
  }

  async #sendCodexRequest<T, M extends string>(
    method: M,
    params?: JsonValue,
    seed?: FactoryCorrelationSeed,
  ): Promise<{ response: T; requestId: string; correlation?: FactoryCorrelation }> {
    if (this.#closing || this.#terminalError) {
      throw this.#terminalError ?? new FactoryClientError('Factory client is closing');
    }
    const id = `codex-${this.#nextRequestId++}`;
    if (seed) this.correlations.begin(seed, id, method, params);
    const outcome = deferred<CodexResponseOutcome>();
    this.#pending.set(id, {
      kind: 'codex',
      id,
      method,
      resolve: outcome.resolve,
      reject: outcome.reject,
    });
    const request: JsonValue = params === undefined
      ? { id, method }
      : { id, method, params };
    try {
      await this.#transport.send(request);
    } catch (error) {
      this.#pending.delete(id);
      throw error;
    }
    const decoded = await outcome.promise;
    if (decoded.type === 'error') throw new CodexRemoteError(id, method, decoded.error);
    const correlation = this.correlations.completeResult(id, method, decoded.result);
    return {
      response: decoded.result as T,
      requestId: id,
      ...(correlation ? { correlation } : {}),
    };
  }

  async #handleMessage(message: JsonValue): Promise<void> {
    const inbound = classifyInbound(message);
    if (inbound.type === 'response') {
      if (typeof inbound.id !== 'string') {
        throw new FactoryClientError(`received numeric response id ${inbound.id} for string-only client request ids`);
      }
      const pending = this.#pending.get(inbound.id);
      if (!pending) throw new FactoryClientError(`received response for unknown request ${inbound.id}`);
      this.#pending.delete(inbound.id);
      if (pending.kind === 'factory') {
        pending.resolve(decodeClientResponse(pending.pending, inbound.value));
      } else {
        pending.resolve(codexResponse(inbound.value, pending.id));
      }
      return;
    }
    if (inbound.type === 'notification') {
      await this.#publishCodexNotification(codexNotification(inbound.method, inbound.params));
      await this.#publishEvent(decodeNotification(inbound.method, inbound.params));
      return;
    }

    const request = decodeServerRequest(inbound.value);
    const requestEvent = await this.#publishEvent({ type: 'serverRequest', request });
    const exactRequest = codexServerRequest(inbound.value);
    await this.#publishCodexServerRequest(exactRequest);
    if (exactRequest.kind === 'known' && this.#options.onCodexServerRequest) {
      let response: Promise<CodexServerRequestResponse> | undefined;
      try {
        response = this.#options.onCodexServerRequest(exactRequest);
      } catch (error) {
        response = Promise.reject(error);
      }
      if (response !== undefined) {
        this.#dispatchCodexServerRequest(exactRequest, response);
        return;
      }
    }
    if (request.kind === 'unknown') {
      const { resolution, wire } = methodNotSupported(request.request);
      await this.#transport.send(wire);
      await this.#emitSignal({
        type: 'terminateOperation',
        resolution,
        ...(requestEvent.correlation ? { correlation: requestEvent.correlation } : {}),
      });
      return;
    }
    if (!this.#options.onServerRequest) {
      const response: FactoryServerErrorResponse = {
        identity: { requestId: request.request.id, method: request.request.method },
        error: {
          code: -32601,
          message: `Factory client has no handler for server request ${request.request.method}`,
          data: { method: request.request.method },
        },
      };
      await this.#transport.send(encodeServerResponse(request.request, response));
      return;
    }
    try {
      const response = await this.#options.onServerRequest(request);
      await this.#transport.send(encodeServerResponse(request.request, response));
    } catch (error) {
      const failure = asError(error);
      this.#options.onError?.(failure);
      const response: FactoryServerErrorResponse = {
        identity: { requestId: request.request.id, method: request.request.method },
        error: { code: -32603, message: failure.message },
      };
      await this.#transport.send(encodeServerResponse(request.request, response));
    }
  }

  async #publishCodexNotification(notification: CodexNotification): Promise<void> {
    const correlation = this.correlations.observeCodexNotification(notification);
    const correlated: FactoryCorrelatedCodexNotification = correlation
      ? { notification, correlation }
      : { notification };
    this.#codexNotificationQueue.push(notification);
    this.#codexCorrelatedNotificationQueue.push(correlated);
    try {
      await this.#options.onCodexNotification?.(notification);
      await this.#options.onCodexCorrelatedNotification?.(correlated);
    } catch (error) {
      this.#options.onError?.(asError(error));
    }
  }

  async #publishCodexServerRequest(request: CodexServerRequestEvent): Promise<void> {
    this.#codexServerRequestQueue.push(request);
  }

  #dispatchCodexServerRequest(
    request: Extract<CodexServerRequestEvent, { kind: 'known' }>,
    response: Promise<CodexServerRequestResponse>,
  ): void {
    const handling = (async () => {
      try {
        const resolved = await response;
        await this.#transport.send(codexServerResponse(request.request.id, resolved));
      } catch (error) {
        const failure = asError(error);
        this.#options.onError?.(failure);
        try {
          await this.#transport.send(codexServerResponse(request.request.id, {
            error: { code: -32603, message: failure.message },
          }));
        } catch (sendError) {
          this.#fail(asError(sendError));
        }
      }
    })();
    this.#inflightCodexServerRequests.add(handling);
    void handling.finally(() => this.#inflightCodexServerRequests.delete(handling));
  }

  async #publishEvent(event: FactoryEvent): Promise<FactoryCorrelatedEvent> {
    const correlation = this.correlations.observe(event);
    const correlated: FactoryCorrelatedEvent = correlation ? { event, correlation } : { event };
    this.#eventQueue.push(correlated);
    try {
      await this.#options.onEvent?.(correlated);
    } catch (error) {
      this.#options.onError?.(asError(error));
    }
    return correlated;
  }

  async #emitSignal(signal: FactoryClientSignal): Promise<void> {
    try {
      await this.#options.onSignal?.(signal);
    } catch (error) {
      this.#options.onError?.(asError(error));
    }
  }

  #fail(error: Error): void {
    if (this.#terminalError) return;
    this.#terminalError = error;
    for (const pending of this.#pending.values()) pending.reject(error);
    this.#pending.clear();
    this.#eventQueue.fail(error);
    this.#codexNotificationQueue.fail(error);
    this.#codexCorrelatedNotificationQueue.fail(error);
    this.#codexServerRequestQueue.fail(error);
    this.#options.onError?.(error);
    this.#settleClosed(error);
    void this.#transport?.closeInput().catch(() => undefined);
  }

  #handleExit(code: number | null, signal: NodeJS.Signals | null, stderr: string): void {
    if (this.#settled) return;
    if (this.#terminalError) {
      this.#settleClosed(this.#terminalError);
      return;
    }
    if (!this.#closing || code !== 0) {
      this.#fail(new FactoryProcessError(
        `factory-runtime exited ${this.#closing ? 'during shutdown' : 'unexpectedly'} with code ${String(code)}`,
        code,
        signal,
        stderr,
      ));
      return;
    }
    const event: FactoryEvent = { type: 'connectionClosed' };
    this.#eventQueue.push({ event });
    this.#eventQueue.end();
    this.#codexNotificationQueue.end();
    this.#codexCorrelatedNotificationQueue.end();
    this.#codexServerRequestQueue.end();
    this.#settleClosed();
  }

  #settleClosed(error?: Error): void {
    if (this.#settled) return;
    this.#settled = true;
    if (error) this.#closedDeferred.reject(error);
    else this.#closedDeferred.resolve();
  }
}

function asError(value: unknown): Error {
  return value instanceof Error ? value : new Error(String(value));
}
