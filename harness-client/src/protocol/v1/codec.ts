import type {
  FactoryDecodedServerRequest,
  FactoryErrorEnvelope,
  FactoryEvent,
  FactoryItem,
  FactoryItemDelta,
  FactoryMethod,
  FactoryMethodNotSupportedResolution,
  FactoryPendingRequest,
  FactoryRequestEnvelope,
  FactoryResponseEnvelope,
  FactoryResponseOutcome,
  FactoryRpcRequestId,
  FactoryServerErrorResponse,
  FactoryServerRequest,
  FactoryServerResponse,
  FactoryThread,
  FactoryThreadStatus,
  FactoryTurn,
  InitializeResponse,
  JsonValue,
  ProtocolManifest,
} from './generated/index.js';

export type AppServerInbound =
  | { type: 'response'; id: FactoryRpcRequestId; value: JsonValue }
  | { type: 'serverRequest'; value: JsonValue }
  | { type: 'notification'; method: string; params: JsonValue };

const KNOWN_SERVER_METHODS = new Set([
  'item/commandExecution/requestApproval',
  'item/fileChange/requestApproval',
  'item/tool/requestUserInput',
  'mcpServer/elicitation/request',
  'item/permissions/requestApproval',
  'item/tool/call',
  'account/chatgptAuthTokens/refresh',
  'attestation/generate',
  'currentTime/read',
]);

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

function boolean(value: unknown, label: string): boolean {
  if (typeof value !== 'boolean') throw new TypeError(`${label} must be a boolean`);
  return value;
}

function optionalString(value: unknown, label: string): string | undefined {
  if (value === undefined || value === null) return undefined;
  return string(value, label);
}

function optionalNumber(value: unknown, label: string): number | undefined {
  if (value === undefined || value === null) return undefined;
  return number(value, label);
}

function nullableString(value: unknown, label: string): string | null {
  if (value === undefined || value === null) return null;
  return string(value, label);
}

function nullableNumber(value: unknown, label: string): number | null {
  if (value === undefined || value === null) return null;
  return number(value, label);
}

function nullableJson(value: unknown): JsonValue | null {
  return value === undefined || value === null ? null : json(value);
}

function rpcId(value: unknown, label: string): FactoryRpcRequestId {
  if (typeof value === 'string') return value;
  if (typeof value === 'number' && Number.isSafeInteger(value)) return value;
  throw new TypeError(`${label} must be a string or safe integer`);
}

function json(value: unknown): JsonValue {
  return value as JsonValue;
}

function stringArray(value: unknown, label: string): string[] {
  if (!Array.isArray(value)) throw new TypeError(`${label} must be an array`);
  return value.map((entry, index) => string(entry, `${label}[${index}]`));
}

function array(value: unknown, label: string): unknown[] {
  if (!Array.isArray(value)) throw new TypeError(`${label} must be an array`);
  return value;
}

function oneOf<const T extends string>(value: unknown, label: string, choices: readonly T[]): T {
  const decoded = string(value, label);
  if (!choices.includes(decoded as T)) {
    throw new TypeError(`${label} must be one of ${choices.join(', ')}`);
  }
  return decoded as T;
}

function optionalProperty<T extends object, K extends string, V>(
  target: T,
  key: K,
  value: V | undefined,
): T & Partial<Record<K, V>> {
  if (value !== undefined) Object.assign(target, { [key]: value });
  return target;
}

export function parseJsonlLine(line: string): JsonValue {
  const value: unknown = JSON.parse(line);
  return json(value);
}

export function classifyInbound(value: JsonValue): AppServerInbound {
  const object = record(value, 'app-server message');
  if ('id' in object && ('result' in object || 'error' in object) && !('method' in object)) {
    return { type: 'response', id: rpcId(object.id, 'app-server response.id'), value };
  }
  if ('id' in object && typeof object.method === 'string') {
    return { type: 'serverRequest', value };
  }
  if (typeof object.method === 'string') {
    return {
      type: 'notification',
      method: object.method,
      params: json(object.params ?? {}),
    };
  }
  throw new TypeError('app-server message is not a response, request, or notification');
}

export function decodeProtocolManifest(value: JsonValue): ProtocolManifest {
  const object = record(value, 'protocol manifest');
  const version = record(object.version, 'protocol manifest version');
  return {
    version: {
      major: number(version.major, 'protocol manifest version.major'),
      minor: number(version.minor, 'protocol manifest version.minor'),
    },
    sourceCodexRevision: string(object.sourceCodexRevision, 'protocol manifest sourceCodexRevision'),
    schemaSha256: string(object.schemaSha256, 'protocol manifest schemaSha256'),
  };
}

export function encodeClientRequest(envelope: FactoryRequestEnvelope): JsonValue {
  const params = { ...record(envelope.params, `${envelope.method} params`) };
  if (envelope.method === 'turn/start') {
    const mode = params.mode;
    delete params.mode;
    if (mode === 'plan') {
      const model = optionalString(params.model, 'turn/start params.model');
      if (model === undefined) throw new TypeError('Factory plan mode requires an explicit model');
      params.collaborationMode = {
        mode: 'plan',
        settings: {
          model,
          reasoning_effort: null,
          developer_instructions: null,
        },
      };
    } else if (mode !== 'normal') {
      throw new TypeError('turn/start params.mode must be normal or plan');
    }
  }
  return json({ id: envelope.id, method: envelope.method, params });
}

export function encodeInitializedNotification(): JsonValue {
  return { method: 'initialized' };
}

export function decodeClientResponse(
  pending: FactoryPendingRequest,
  raw: JsonValue,
): FactoryResponseOutcome {
  const object = record(raw, 'app-server response');
  if (object.id !== pending.id) {
    throw new TypeError(`app-server response id does not match pending request ${pending.id}`);
  }
  if ('error' in object) {
    const error = record(object.error, 'app-server response error');
    const envelope: FactoryErrorEnvelope = {
      id: pending.id,
      method: pending.method,
      error: {
        code: number(error.code, 'app-server response error.code'),
        message: string(error.message, 'app-server response error.message'),
      },
    };
    if (error.data !== undefined && error.data !== null) envelope.error.data = json(error.data);
    return { type: 'error', error: envelope };
  }
  if (!('result' in object)) throw new TypeError('app-server response has neither result nor error');
  const result = decodeResult(pending.method, json(object.result));
  const response = { id: pending.id, method: pending.method, result } as FactoryResponseEnvelope;
  return { type: 'success', response };
}

function decodeResult(method: FactoryMethod, value: JsonValue): JsonValue {
  const object = record(value, `${method} result`);
  switch (method) {
    case 'initialize': {
      const response: InitializeResponse = {
        userAgent: string(object.userAgent, 'initialize result.userAgent'),
        codexHome: string(object.codexHome, 'initialize result.codexHome'),
        platformFamily: string(object.platformFamily, 'initialize result.platformFamily'),
        platformOs: string(object.platformOs, 'initialize result.platformOs'),
      };
      return json(response);
    }
    case 'thread/start':
    case 'thread/resume':
    case 'thread/fork':
      return json({ ...object, thread: projectThread(object.thread), raw: value });
    case 'turn/start':
      return json({ ...object, turn: projectTurn(object.turn) });
    case 'thread/compact/start':
    case 'turn/steer':
    case 'turn/interrupt':
      return value;
  }
}

function projectThreadStatus(value: unknown): FactoryThreadStatus {
  const raw = json(value);
  try {
    const object = record(value, 'thread status');
    const type = string(object.type, 'thread status.type');
    if (type === 'notLoaded' || type === 'idle' || type === 'systemError') return { type, raw };
    if (type === 'active') {
      const activeFlags = stringArray(object.activeFlags, 'thread status.activeFlags');
      if (!activeFlags.every((flag) => flag === 'waitingOnApproval' || flag === 'waitingOnUserInput')) {
        throw new TypeError('thread status contains an unsupported active flag');
      }
      return { type, activeFlags: activeFlags as Array<'waitingOnApproval' | 'waitingOnUserInput'>, raw };
    }
    return { type: 'unknown', upstreamStatus: type, raw };
  } catch {
    const rawType = typeof value === 'object' && value !== null
      ? (value as Record<string, unknown>).type
      : undefined;
    const upstreamStatus = typeof rawType === 'string' ? rawType : '<unknown>';
    return { type: 'unknown', upstreamStatus, raw };
  }
}

function projectThread(value: unknown): FactoryThread {
  const object = record(value, 'thread');
  const thread: FactoryThread = {
    id: string(object.id, 'thread.id'),
    sessionId: string(object.sessionId, 'thread.sessionId'),
    preview: string(object.preview, 'thread.preview'),
    ephemeral: boolean(object.ephemeral, 'thread.ephemeral'),
    modelProvider: string(object.modelProvider, 'thread.modelProvider'),
    createdAt: number(object.createdAt, 'thread.createdAt'),
    updatedAt: number(object.updatedAt, 'thread.updatedAt'),
    status: projectThreadStatus(object.status),
    cwd: string(object.cwd, 'thread.cwd'),
    raw: json(value),
  };
  optionalProperty(thread, 'forkedFromId', optionalString(object.forkedFromId, 'thread.forkedFromId'));
  optionalProperty(thread, 'parentThreadId', optionalString(object.parentThreadId, 'thread.parentThreadId'));
  return thread;
}

function projectTurn(value: unknown): FactoryTurn {
  const object = record(value, 'turn');
  if (!Array.isArray(object.items)) throw new TypeError('turn.items must be an array');
  const turn: FactoryTurn = {
    id: string(object.id, 'turn.id'),
    items: object.items.map(projectItem),
    status: string(object.status, 'turn.status') as FactoryTurn['status'],
    raw: json(value),
  };
  if (object.error !== undefined && object.error !== null) {
    const error = record(object.error, 'turn.error');
    const projectedError: NonNullable<FactoryTurn['error']> = {
      message: string(error.message, 'turn.error.message'),
    };
    if (error.codexErrorInfo !== undefined && error.codexErrorInfo !== null) {
      projectedError.codexErrorInfo = json(error.codexErrorInfo);
    }
    optionalProperty(projectedError, 'additionalDetails', optionalString(error.additionalDetails, 'turn.error.additionalDetails'));
    turn.error = projectedError;
  }
  optionalProperty(turn, 'startedAt', optionalNumber(object.startedAt, 'turn.startedAt'));
  optionalProperty(turn, 'completedAt', optionalNumber(object.completedAt, 'turn.completedAt'));
  optionalProperty(turn, 'durationMs', optionalNumber(object.durationMs, 'turn.durationMs'));
  return turn;
}

function projectItem(value: unknown): FactoryItem {
  const raw = json(value);
  try {
    const object = record(value, 'item');
    const type = string(object.type, 'item.type');
    const id = string(object.id, 'item.id');
    switch (type) {
      case 'userMessage': {
        if (!Array.isArray(object.content)) throw new TypeError('item.content must be an array');
        const item: Extract<FactoryItem, { type: 'userMessage' }> = {
          type,
          id,
          content: object.content as Extract<FactoryItem, { type: 'userMessage' }>['content'],
          raw,
        };
        optionalProperty(item, 'clientId', optionalString(object.clientId, 'item.clientId'));
        return item;
      }
      case 'agentMessage': {
        const item: Extract<FactoryItem, { type: 'agentMessage' }> = {
          type,
          id,
          text: string(object.text, 'item.text'),
          raw,
        };
        optionalProperty(item, 'phase', optionalString(object.phase, 'item.phase'));
        if (object.memoryCitation !== undefined && object.memoryCitation !== null) {
          item.memoryCitation = json(object.memoryCitation);
        }
        return item;
      }
      case 'plan':
        return { type, id, text: string(object.text, 'item.text'), raw };
      case 'reasoning':
        return {
          type,
          id,
          summary: stringArray(object.summary, 'item.summary'),
          content: stringArray(object.content, 'item.content'),
          raw,
        };
      case 'commandExecution': {
        const item: Extract<FactoryItem, { type: 'commandExecution' }> = {
          type,
          id,
          command: string(object.command, 'item.command'),
          cwd: string(object.cwd, 'item.cwd'),
          status: string(object.status, 'item.status'),
          raw,
        };
        optionalProperty(item, 'processId', optionalString(object.processId, 'item.processId'));
        optionalProperty(item, 'aggregatedOutput', optionalString(object.aggregatedOutput, 'item.aggregatedOutput'));
        optionalProperty(item, 'exitCode', optionalNumber(object.exitCode, 'item.exitCode'));
        optionalProperty(item, 'durationMs', optionalNumber(object.durationMs, 'item.durationMs'));
        return item;
      }
      case 'fileChange':
        return {
          type,
          id,
          changes: json(object.changes),
          status: string(object.status, 'item.status'),
          raw,
        };
      case 'mcpToolCall': {
        const item: Extract<FactoryItem, { type: 'mcpToolCall' }> = {
          type,
          id,
          server: string(object.server, 'item.server'),
          tool: string(object.tool, 'item.tool'),
          status: string(object.status, 'item.status'),
          arguments: json(object.arguments),
          raw,
        };
        if (object.result !== undefined && object.result !== null) item.result = json(object.result);
        if (object.error !== undefined && object.error !== null) item.error = json(object.error);
        optionalProperty(item, 'durationMs', optionalNumber(object.durationMs, 'item.durationMs'));
        return item;
      }
      case 'dynamicToolCall': {
        const item: Extract<FactoryItem, { type: 'dynamicToolCall' }> = {
          type,
          id,
          tool: string(object.tool, 'item.tool'),
          arguments: json(object.arguments),
          status: string(object.status, 'item.status'),
          raw,
        };
        optionalProperty(item, 'namespace', optionalString(object.namespace, 'item.namespace'));
        if (object.contentItems !== undefined && object.contentItems !== null) item.contentItems = json(object.contentItems);
        if (object.success !== undefined && object.success !== null) item.success = boolean(object.success, 'item.success');
        optionalProperty(item, 'durationMs', optionalNumber(object.durationMs, 'item.durationMs'));
        return item;
      }
      case 'collabAgentToolCall': {
        const item: Extract<FactoryItem, { type: 'collabAgentToolCall' }> = {
          type,
          id,
          tool: string(object.tool, 'item.tool'),
          status: string(object.status, 'item.status'),
          senderThreadId: string(object.senderThreadId, 'item.senderThreadId'),
          receiverThreadIds: stringArray(object.receiverThreadIds, 'item.receiverThreadIds'),
          agentsStates: json(object.agentsStates),
          raw,
        };
        optionalProperty(item, 'prompt', optionalString(object.prompt, 'item.prompt'));
        optionalProperty(item, 'model', optionalString(object.model, 'item.model'));
        optionalProperty(item, 'reasoningEffort', optionalString(object.reasoningEffort, 'item.reasoningEffort'));
        return item;
      }
      case 'contextCompaction':
        return { type, id, raw };
      default:
        return { type: 'unknown', id, upstreamType: type, value: raw };
    }
  } catch {
    const object = typeof value === 'object' && value !== null && !Array.isArray(value)
      ? value as Record<string, unknown>
      : {};
    const id = typeof object.id === 'string' ? object.id : undefined;
    const upstreamType = typeof object.type === 'string' ? object.type : '<unknown>';
    const unknown: Extract<FactoryItem, { type: 'unknown' }> = {
      type: 'unknown',
      upstreamType,
      value: raw,
    };
    optionalProperty(unknown, 'id', id);
    return unknown;
  }
}

export function decodeNotification(method: string, params: JsonValue): FactoryEvent {
  try {
    const object = record(params, `${method} params`);
    switch (method) {
      case 'thread/started':
        return { type: 'threadStarted', thread: projectThread(object.thread) };
      case 'thread/status/changed':
        return {
          type: 'threadStatusChanged',
          threadId: string(object.threadId, `${method} params.threadId`),
          status: projectThreadStatus(object.status),
        };
      case 'thread/compacted':
        return {
          type: 'threadCompacted',
          threadId: string(object.threadId, `${method} params.threadId`),
          turnId: string(object.turnId, `${method} params.turnId`),
        };
      case 'turn/started':
      case 'turn/completed':
        return {
          type: method === 'turn/started' ? 'turnStarted' : 'turnCompleted',
          threadId: string(object.threadId, `${method} params.threadId`),
          turn: projectTurn(object.turn),
        };
      case 'turn/plan/updated': {
        if (!Array.isArray(object.plan)) throw new TypeError(`${method} params.plan must be an array`);
        const event: Extract<FactoryEvent, { type: 'turnPlanUpdated' }> = {
          type: 'turnPlanUpdated',
          threadId: string(object.threadId, `${method} params.threadId`),
          turnId: string(object.turnId, `${method} params.turnId`),
          plan: object.plan as Extract<FactoryEvent, { type: 'turnPlanUpdated' }>['plan'],
        };
        optionalProperty(event, 'explanation', optionalString(object.explanation, `${method} params.explanation`));
        return event;
      }
      case 'item/started':
      case 'item/completed':
        return method === 'item/started'
          ? {
              type: 'itemStarted',
              threadId: string(object.threadId, `${method} params.threadId`),
              turnId: string(object.turnId, `${method} params.turnId`),
              item: projectItem(object.item),
              startedAtMs: number(object.startedAtMs, `${method} params.startedAtMs`),
            }
          : {
              type: 'itemCompleted',
              threadId: string(object.threadId, `${method} params.threadId`),
              turnId: string(object.turnId, `${method} params.turnId`),
              item: projectItem(object.item),
              completedAtMs: number(object.completedAtMs, `${method} params.completedAtMs`),
            };
      case 'serverRequest/resolved':
        return {
          type: 'serverRequestResolved',
          threadId: string(object.threadId, `${method} params.threadId`),
          requestId: rpcId(object.requestId, `${method} params.requestId`),
        };
      case 'error': {
        const error = record(object.error, `${method} params.error`);
        const runtimeError: Extract<FactoryEvent, { type: 'runtimeError' }>['error'] = {
          message: string(error.message, `${method} params.error.message`),
          willRetry: boolean(object.willRetry, `${method} params.willRetry`),
          threadId: string(object.threadId, `${method} params.threadId`),
          turnId: string(object.turnId, `${method} params.turnId`),
        };
        if (error.codexErrorInfo !== undefined && error.codexErrorInfo !== null) {
          runtimeError.codexErrorInfo = json(error.codexErrorInfo);
        }
        optionalProperty(runtimeError, 'additionalDetails', optionalString(error.additionalDetails, `${method} params.error.additionalDetails`));
        return { type: 'runtimeError', error: runtimeError };
      }
      default:
        return decodeItemDelta(method, object, params);
    }
  } catch {
    return { type: 'unknownNotification', method, params };
  }
}

function decodeItemDelta(
  method: string,
  params: Record<string, unknown>,
  raw: JsonValue,
): FactoryEvent {
  let delta: FactoryItemDelta;
  switch (method) {
    case 'item/agentMessage/delta':
      delta = { type: 'agentMessage', delta: string(params.delta, `${method} params.delta`), raw };
      break;
    case 'item/plan/delta':
      delta = { type: 'plan', delta: string(params.delta, `${method} params.delta`), raw };
      break;
    case 'item/reasoning/summaryTextDelta':
      delta = {
        type: 'reasoningSummaryText',
        delta: string(params.delta, `${method} params.delta`),
        summaryIndex: number(params.summaryIndex, `${method} params.summaryIndex`),
        raw,
      };
      break;
    case 'item/reasoning/summaryPartAdded':
      delta = {
        type: 'reasoningSummaryPartAdded',
        summaryIndex: number(params.summaryIndex, `${method} params.summaryIndex`),
        raw,
      };
      break;
    case 'item/reasoning/textDelta':
      delta = {
        type: 'reasoningText',
        delta: string(params.delta, `${method} params.delta`),
        contentIndex: number(params.contentIndex, `${method} params.contentIndex`),
        raw,
      };
      break;
    case 'item/commandExecution/outputDelta':
      delta = { type: 'commandExecutionOutput', delta: string(params.delta, `${method} params.delta`), raw };
      break;
    case 'item/fileChange/outputDelta':
      delta = { type: 'fileChangeOutput', delta: string(params.delta, `${method} params.delta`), raw };
      break;
    case 'item/fileChange/patchUpdated':
      delta = { type: 'fileChangePatchUpdated', changes: json(params.changes), raw };
      break;
    case 'item/mcpToolCall/progress':
      delta = { type: 'mcpToolCallProgress', message: string(params.message, `${method} params.message`), raw };
      break;
    case 'item/commandExecution/terminalInteraction':
      delta = {
        type: 'terminalInteraction',
        processId: string(params.processId, `${method} params.processId`),
        stdin: string(params.stdin, `${method} params.stdin`),
        raw,
      };
      break;
    default:
      delta = { type: 'unknown', upstreamMethod: method, value: raw };
  }
  return {
    type: 'itemDelta',
    threadId: string(params.threadId, `${method} params.threadId`),
    turnId: string(params.turnId, `${method} params.turnId`),
    itemId: string(params.itemId, `${method} params.itemId`),
    delta,
  };
}

export function decodeServerRequest(raw: JsonValue): FactoryDecodedServerRequest {
  const object = record(raw, 'app-server server request');
  const id = rpcId(object.id, 'app-server server request.id');
  const method = string(object.method, 'app-server server request.method');
  const params = json(object.params ?? {});
  const rawRequest = { id, method, params };
  if (!KNOWN_SERVER_METHODS.has(method)) {
    return { kind: 'unknown', request: rawRequest };
  }
  const request = decodeKnownServerRequest(id, method, params);
  return { kind: 'known', request, raw: rawRequest };
}

type CommandApprovalParams = Extract<
  FactoryServerRequest,
  { method: 'item/commandExecution/requestApproval' }
>['params'];
type CommandApprovalDecision = NonNullable<CommandApprovalParams['availableDecisions']>[number];
type McpElicitationParams = Extract<
  FactoryServerRequest,
  { method: 'mcpServer/elicitation/request' }
>['params'];

function decodeKnownServerRequest(
  id: FactoryRpcRequestId,
  method: string,
  rawParams: JsonValue,
): FactoryServerRequest {
  const params = record(rawParams, `${method} params`);
  switch (method) {
    case 'item/commandExecution/requestApproval':
      return { method, id, params: decodeCommandApprovalParams(params, method) };
    case 'item/fileChange/requestApproval':
      return {
        method,
        id,
        params: {
          threadId: string(params.threadId, `${method} params.threadId`),
          turnId: string(params.turnId, `${method} params.turnId`),
          itemId: string(params.itemId, `${method} params.itemId`),
          startedAtMs: number(params.startedAtMs, `${method} params.startedAtMs`),
          reason: nullableString(params.reason, `${method} params.reason`),
          grantRoot: nullableString(params.grantRoot, `${method} params.grantRoot`),
        },
      };
    case 'item/tool/requestUserInput':
      return {
        method,
        id,
        params: {
          threadId: string(params.threadId, `${method} params.threadId`),
          turnId: string(params.turnId, `${method} params.turnId`),
          itemId: string(params.itemId, `${method} params.itemId`),
          questions: array(params.questions, `${method} params.questions`).map((value, index) => {
            const question = record(value, `${method} params.questions[${index}]`);
            const options = question.options === undefined || question.options === null
              ? null
              : array(question.options, `${method} params.questions[${index}].options`).map(
                  (optionValue, optionIndex) => {
                    const option = record(
                      optionValue,
                      `${method} params.questions[${index}].options[${optionIndex}]`,
                    );
                    return {
                      label: string(option.label, `${method} question option.label`),
                      description: string(option.description, `${method} question option.description`),
                    };
                  },
                );
            return {
              id: string(question.id, `${method} question.id`),
              header: string(question.header, `${method} question.header`),
              question: string(question.question, `${method} question.question`),
              isOther: question.isOther === undefined
                ? false
                : boolean(question.isOther, `${method} question.isOther`),
              isSecret: question.isSecret === undefined
                ? false
                : boolean(question.isSecret, `${method} question.isSecret`),
              options,
            };
          }),
          autoResolutionMs: nullableNumber(
            params.autoResolutionMs,
            `${method} params.autoResolutionMs`,
          ),
        },
      };
    case 'mcpServer/elicitation/request':
      return { method, id, params: decodeMcpElicitationParams(params, method) };
    case 'item/permissions/requestApproval':
      return {
        method,
        id,
        params: {
          threadId: string(params.threadId, `${method} params.threadId`),
          turnId: string(params.turnId, `${method} params.turnId`),
          itemId: string(params.itemId, `${method} params.itemId`),
          environmentId: nullableString(params.environmentId, `${method} params.environmentId`),
          startedAtMs: number(params.startedAtMs, `${method} params.startedAtMs`),
          cwd: string(params.cwd, `${method} params.cwd`),
          reason: nullableString(params.reason, `${method} params.reason`),
          permissions: json(params.permissions),
        },
      };
    case 'item/tool/call':
      return {
        method,
        id,
        params: {
          threadId: string(params.threadId, `${method} params.threadId`),
          turnId: string(params.turnId, `${method} params.turnId`),
          callId: string(params.callId, `${method} params.callId`),
          namespace: nullableString(params.namespace, `${method} params.namespace`),
          tool: string(params.tool, `${method} params.tool`),
          arguments: json(params.arguments),
        },
      };
    case 'account/chatgptAuthTokens/refresh':
      return {
        method,
        id,
        params: {
          reason: oneOf(params.reason, `${method} params.reason`, ['unauthorized']),
          previousAccountId: nullableString(
            params.previousAccountId,
            `${method} params.previousAccountId`,
          ),
        },
      };
    case 'attestation/generate':
      return { method, id, params: {} };
    case 'currentTime/read':
      return {
        method,
        id,
        params: { threadId: string(params.threadId, `${method} params.threadId`) },
      };
    default:
      throw new TypeError(`known server request method has no Factory decoder: ${method}`);
  }
}

function decodeCommandApprovalParams(
  params: Record<string, unknown>,
  method: string,
): CommandApprovalParams {
  const decoded: CommandApprovalParams = {
    threadId: string(params.threadId, `${method} params.threadId`),
    turnId: string(params.turnId, `${method} params.turnId`),
    itemId: string(params.itemId, `${method} params.itemId`),
    startedAtMs: number(params.startedAtMs, `${method} params.startedAtMs`),
    environmentId: nullableString(params.environmentId, `${method} params.environmentId`),
  };
  optionalProperty(decoded, 'approvalId', optionalString(params.approvalId, `${method} params.approvalId`));
  optionalProperty(decoded, 'reason', optionalString(params.reason, `${method} params.reason`));
  if (params.networkApprovalContext !== undefined && params.networkApprovalContext !== null) {
    decoded.networkApprovalContext = json(params.networkApprovalContext);
  }
  optionalProperty(decoded, 'command', optionalString(params.command, `${method} params.command`));
  optionalProperty(decoded, 'cwd', optionalString(params.cwd, `${method} params.cwd`));
  if (params.commandActions !== undefined && params.commandActions !== null) {
    decoded.commandActions = array(params.commandActions, `${method} params.commandActions`).map(json);
  }
  if (params.additionalPermissions !== undefined && params.additionalPermissions !== null) {
    decoded.additionalPermissions = json(params.additionalPermissions);
  }
  if (params.proposedExecpolicyAmendment !== undefined && params.proposedExecpolicyAmendment !== null) {
    decoded.proposedExecpolicyAmendment = stringArray(
      params.proposedExecpolicyAmendment,
      `${method} params.proposedExecpolicyAmendment`,
    );
  }
  if (params.proposedNetworkPolicyAmendments !== undefined &&
    params.proposedNetworkPolicyAmendments !== null) {
    decoded.proposedNetworkPolicyAmendments = array(
      params.proposedNetworkPolicyAmendments,
      `${method} params.proposedNetworkPolicyAmendments`,
    ).map((value, index) => decodeNetworkPolicyAmendment(
      value,
      `${method} params.proposedNetworkPolicyAmendments[${index}]`,
    ));
  }
  if (params.availableDecisions !== undefined && params.availableDecisions !== null) {
    decoded.availableDecisions = array(
      params.availableDecisions,
      `${method} params.availableDecisions`,
    ).map((value, index) => decodeCommandDecision(
      value,
      `${method} params.availableDecisions[${index}]`,
    ));
  }
  return decoded;
}

function decodeNetworkPolicyAmendment(
  value: unknown,
  label: string,
): { host: string; action: 'allow' | 'deny' } {
  const amendment = record(value, label);
  return {
    host: string(amendment.host, `${label}.host`),
    action: oneOf(amendment.action, `${label}.action`, ['allow', 'deny']),
  };
}

function decodeCommandDecision(value: unknown, label: string): CommandApprovalDecision {
  if (typeof value === 'string') {
    return oneOf(value, label, ['accept', 'acceptForSession', 'decline', 'cancel']);
  }
  const decision = record(value, label);
  if ('acceptWithExecpolicyAmendment' in decision) {
    const payload = record(
      decision.acceptWithExecpolicyAmendment,
      `${label}.acceptWithExecpolicyAmendment`,
    );
    return {
      acceptWithExecpolicyAmendment: {
        execpolicyAmendment: stringArray(
          payload.execpolicyAmendment ?? payload.execpolicy_amendment,
          `${label}.acceptWithExecpolicyAmendment.execpolicyAmendment`,
        ),
      },
    };
  }
  if ('applyNetworkPolicyAmendment' in decision) {
    const payload = record(
      decision.applyNetworkPolicyAmendment,
      `${label}.applyNetworkPolicyAmendment`,
    );
    return {
      applyNetworkPolicyAmendment: {
        networkPolicyAmendment: decodeNetworkPolicyAmendment(
          payload.networkPolicyAmendment ?? payload.network_policy_amendment,
          `${label}.applyNetworkPolicyAmendment.networkPolicyAmendment`,
        ),
      },
    };
  }
  throw new TypeError(`${label} is not a supported command approval decision`);
}

function decodeMcpElicitationParams(
  params: Record<string, unknown>,
  method: string,
): McpElicitationParams {
  const common = {
    threadId: string(params.threadId, `${method} params.threadId`),
    turnId: nullableString(params.turnId, `${method} params.turnId`),
    serverName: string(params.serverName, `${method} params.serverName`),
    _meta: nullableJson(params._meta),
    message: string(params.message, `${method} params.message`),
  };
  const mode = oneOf(params.mode, `${method} params.mode`, ['form', 'openai/form', 'url']);
  if (mode === 'url') {
    return {
      ...common,
      mode,
      url: string(params.url, `${method} params.url`),
      elicitationId: string(params.elicitationId, `${method} params.elicitationId`),
    };
  }
  return {
    ...common,
    mode,
    requestedSchema: json(params.requestedSchema),
  };
}

export function encodeServerResponse(
  request: FactoryServerRequest,
  response: FactoryServerResponse | FactoryServerErrorResponse,
): JsonValue {
  if ('identity' in response) {
    if (response.identity.requestId !== request.id || response.identity.method !== request.method) {
      throw new TypeError('Factory server error response does not match its request');
    }
    const error = record(response.error, 'Factory server error response.error');
    const wireError: Record<string, JsonValue> = {
      code: number(error.code, 'Factory server error response.error.code'),
      message: string(error.message, 'Factory server error response.error.message'),
    };
    if (error.data !== undefined && error.data !== null) wireError.data = json(error.data);
    return json({ id: request.id, error: wireError });
  }
  if (response.id !== request.id || response.method !== request.method) {
    throw new TypeError('Factory server response does not match its request');
  }
  return json({ id: request.id, result: encodeKnownServerResult(response) });
}

function encodeKnownServerResult(response: FactoryServerResponse): JsonValue {
  const value = record(response.response, `${response.method} response`);
  switch (response.method) {
    case 'item/commandExecution/requestApproval':
      return json({
        decision: decodeCommandDecision(
          value.decision,
          `${response.method} response.decision`,
        ),
      });
    case 'item/fileChange/requestApproval':
      return json({
        decision: oneOf(
          value.decision,
          `${response.method} response.decision`,
          ['accept', 'acceptForSession', 'decline', 'cancel'],
        ),
      });
    case 'item/tool/requestUserInput': {
      const answers = record(value.answers, `${response.method} response.answers`);
      return json({
        answers: Object.fromEntries(Object.entries(answers).map(([questionId, rawAnswer]) => {
          const answer = record(rawAnswer, `${response.method} response.answers.${questionId}`);
          return [questionId, {
            answers: stringArray(
              answer.answers,
              `${response.method} response.answers.${questionId}.answers`,
            ),
          }];
        })),
      });
    }
    case 'mcpServer/elicitation/request':
      return json({
        action: oneOf(
          value.action,
          `${response.method} response.action`,
          ['accept', 'decline', 'cancel'],
        ),
        content: nullableJson(value.content),
        _meta: nullableJson(value._meta),
      });
    case 'item/permissions/requestApproval': {
      const result: Record<string, JsonValue> = {
        permissions: json(value.permissions),
        scope: oneOf(value.scope, `${response.method} response.scope`, ['turn', 'session']),
      };
      if (value.strictAutoReview !== undefined && value.strictAutoReview !== null) {
        result.strictAutoReview = boolean(
          value.strictAutoReview,
          `${response.method} response.strictAutoReview`,
        );
      }
      return result;
    }
    case 'item/tool/call':
      return json({
        contentItems: array(
          value.contentItems,
          `${response.method} response.contentItems`,
        ).map((item, index) => decodeDynamicToolContentItem(
          item,
          `${response.method} response.contentItems[${index}]`,
        )),
        success: boolean(value.success, `${response.method} response.success`),
      });
    case 'account/chatgptAuthTokens/refresh':
      return json({
        accessToken: string(value.accessToken, `${response.method} response.accessToken`),
        chatgptAccountId: string(
          value.chatgptAccountId,
          `${response.method} response.chatgptAccountId`,
        ),
        chatgptPlanType: nullableString(
          value.chatgptPlanType,
          `${response.method} response.chatgptPlanType`,
        ),
      });
    case 'attestation/generate':
      return json({ token: string(value.token, `${response.method} response.token`) });
    case 'currentTime/read':
      return json({
        currentTimeAt: number(value.currentTimeAt, `${response.method} response.currentTimeAt`),
      });
  }
}

function decodeDynamicToolContentItem(
  value: unknown,
  label: string,
): JsonValue {
  const item = record(value, label);
  const type = oneOf(item.type, `${label}.type`, ['inputText', 'inputImage', 'inputAudio']);
  if (type === 'inputText') return { type, text: string(item.text, `${label}.text`) };
  if (type === 'inputImage') {
    return { type, imageUrl: string(item.imageUrl, `${label}.imageUrl`) };
  }
  return { type, audioUrl: string(item.audioUrl, `${label}.audioUrl`) };
}

export function methodNotSupported(
  request: Extract<FactoryDecodedServerRequest, { kind: 'unknown' }>['request'],
): { resolution: FactoryMethodNotSupportedResolution; wire: JsonValue } {
  const response: FactoryServerErrorResponse = {
    identity: { requestId: request.id, method: request.method },
    error: {
      code: -32601,
      message: `server request method is not supported: ${request.method}`,
      data: { method: request.method },
    },
  };
  return {
    resolution: { response, terminateOperation: true },
    wire: json({ id: request.id, error: response.error }),
  };
}
