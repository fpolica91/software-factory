import type { ClientRequest } from './generated/ClientRequest.js';
import type { CollaborationMode } from './generated/CollaborationMode.js';
import type { RequestId } from './generated/RequestId.js';
import type { ServerNotification } from './generated/ServerNotification.js';
import type { ServerRequest } from './generated/ServerRequest.js';
import { CODEX_V2_PROTOCOL_MANIFEST } from './generated/factoryManifest.js';
import type { JsonValue } from './generated/serde_json/JsonValue.js';
import type { ReviewStartResponse } from './generated/v2/ReviewStartResponse.js';
import type { ThreadForkResponse } from './generated/v2/ThreadForkResponse.js';
import type { ThreadResumeResponse } from './generated/v2/ThreadResumeResponse.js';
import type { ThreadStartResponse } from './generated/v2/ThreadStartResponse.js';
import type { TurnInterruptResponse } from './generated/v2/TurnInterruptResponse.js';
import type { TurnStartParams } from './generated/v2/TurnStartParams.js';
import type { TurnStartResponse } from './generated/v2/TurnStartResponse.js';

export type CodexClientMethod = ClientRequest['method'];

type CodexGeneratedClientRequestParams<M extends CodexClientMethod> = Extract<
  ClientRequest,
  { method: M }
>['params'];

/** Experimental field intentionally omitted from the stable generated TurnStartParams. */
export type CodexExperimentalTurnStartParams = TurnStartParams & {
  collaborationMode?: CollaborationMode | null;
};

export type CodexClientRequestParams<M extends CodexClientMethod> =
  M extends 'turn/start'
    ? CodexExperimentalTurnStartParams
    : CodexGeneratedClientRequestParams<M>;

export type CodexClientRequestArguments<M extends CodexClientMethod> =
  [CodexClientRequestParams<M>] extends [undefined]
    ? [params?: undefined]
    : [params: CodexClientRequestParams<M>];

interface CodexKnownClientResultMap {
  'thread/start': ThreadStartResponse;
  'thread/resume': ThreadResumeResponse;
  'thread/fork': ThreadForkResponse;
  'turn/start': TurnStartResponse;
  'turn/interrupt': TurnInterruptResponse;
  'review/start': ReviewStartResponse;
}

/** Exact generated result where Factory actively consumes it, raw JSON otherwise. */
export type CodexClientResult<M extends CodexClientMethod> =
  M extends keyof CodexKnownClientResultMap ? CodexKnownClientResultMap[M] : JsonValue;

export interface CodexRawNotification {
  method: string;
  params: JsonValue;
}

export type CodexNotification =
  | { kind: 'known'; notification: ServerNotification }
  | { kind: 'raw'; notification: CodexRawNotification };

export interface CodexRawServerRequest {
  id: RequestId;
  method: string;
  params: JsonValue;
}

export type CodexServerRequestEvent =
  | { kind: 'known'; request: ServerRequest }
  | { kind: 'raw'; request: CodexRawServerRequest };

export interface CodexRpcError {
  code: number;
  message: string;
  data?: JsonValue;
}

export type CodexResponseOutcome =
  | { type: 'success'; result: JsonValue }
  | { type: 'error'; error: CodexRpcError };

export type CodexServerRequestResponse =
  | { result: JsonValue }
  | { error: CodexRpcError };

const notificationMethods = new Set<string>(
  CODEX_V2_PROTOCOL_MANIFEST.serverNotificationMethods,
);
const serverRequestMethods = new Set<string>(CODEX_V2_PROTOCOL_MANIFEST.serverRequestMethods);

export function codexNotification(method: string, params: JsonValue): CodexNotification {
  const notification = { method, params };
  if (notificationMethods.has(method)) {
    return { kind: 'known', notification: notification as unknown as ServerNotification };
  }
  return { kind: 'raw', notification };
}

export function codexServerRequest(value: JsonValue): CodexServerRequestEvent {
  const object = record(value, 'Codex server request');
  const method = string(object.method, 'Codex server request.method');
  const request: CodexRawServerRequest = {
    id: requestId(object.id, 'Codex server request.id'),
    method,
    params: json(object.params ?? {}),
  };
  if (serverRequestMethods.has(method)) {
    return { kind: 'known', request: request as unknown as ServerRequest };
  }
  return { kind: 'raw', request };
}

export function codexResponse(value: JsonValue, expectedId: string): CodexResponseOutcome {
  const object = record(value, 'Codex response');
  if (object.id !== expectedId) {
    throw new TypeError(`Codex response id does not match pending request ${expectedId}`);
  }
  if ('error' in object) {
    const error = record(object.error, 'Codex response.error');
    const decoded: CodexRpcError = {
      code: number(error.code, 'Codex response.error.code'),
      message: string(error.message, 'Codex response.error.message'),
    };
    if (error.data !== undefined && error.data !== null) decoded.data = json(error.data);
    return { type: 'error', error: decoded };
  }
  if (!('result' in object)) throw new TypeError('Codex response has neither result nor error');
  return { type: 'success', result: json(object.result) };
}

export function codexServerResponse(
  id: RequestId,
  response: CodexServerRequestResponse,
): JsonValue {
  return json('result' in response
    ? { id, result: response.result }
    : { id, error: response.error });
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new TypeError(`${label} must be a JSON object`);
  }
  return value as Record<string, unknown>;
}

function string(value: unknown, label: string): string {
  if (typeof value !== 'string') throw new TypeError(`${label} must be a string`);
  return value;
}

function number(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new TypeError(`${label} must be a finite number`);
  }
  return value;
}

function requestId(value: unknown, label: string): RequestId {
  if (typeof value === 'string') return value;
  if (typeof value === 'number' && Number.isSafeInteger(value)) return value;
  throw new TypeError(`${label} must be a string or safe integer`);
}

function json(value: unknown): JsonValue {
  return value as JsonValue;
}
