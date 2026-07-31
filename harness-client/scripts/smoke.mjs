import { access, mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  FactoryClient,
  FactoryRemoteError,
  FACTORY_PROTOCOL_MANIFEST,
} from '../dist/index.js';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const defaultRuntime = resolve(
  packageRoot,
  '..',
  'factory-harness',
  'factory',
  'target',
  'debug',
  'factory-runtime',
);
const runtimePath = process.env.FACTORY_RUNTIME_PATH ?? defaultRuntime;
await access(runtimePath);

const temporaryRoot = await mkdtemp(resolve(tmpdir(), 'factory-harness-client-smoke-'));
const codexHome = resolve(temporaryRoot, 'codex-home');
const workspace = resolve(temporaryRoot, 'workspace');
await mkdir(codexHome, { recursive: true });
await mkdir(workspace, { recursive: true });

const seed = {
  jobId: 'smoke-job',
  operationId: 'smoke-operation',
  attemptId: 'smoke-attempt-1',
  workflowRunId: 'smoke-workflow-run',
  taskRunExternalId: 'smoke-task-run',
};

let client;
const observedEvents = [];
try {
  client = await FactoryClient.connect({
    runtimePath,
    cwd: workspace,
    env: { CODEX_HOME: codexHome },
    onEvent: ({ event, correlation }) => {
      observedEvents.push({
        type: event.type,
        requestId: correlation?.requestId ?? null,
        attemptId: correlation?.attemptId ?? null,
      });
    },
  });
  if (client.manifest.factoryProtocol.schemaSha256 !== FACTORY_PROTOCOL_MANIFEST.schemaSha256) {
    throw new Error('negotiated manifest does not match generated protocol');
  }
  if (client.initializeResponse.codexHome !== codexHome) {
    throw new Error('runtime initialized with an unexpected Codex home');
  }

  const started = await client.startThread({ cwd: workspace }, seed);
  const threadId = started.response.thread.id;
  if (started.correlation.threadId !== threadId) {
    throw new Error('thread/start did not populate durable correlation');
  }

  await client.compactThread({ threadId }, { ...seed, attemptId: 'smoke-attempt-2' });
  let resumeOutcome = 'resumed';
  try {
    const resumed = await client.resumeThread(
      { threadId },
      { ...seed, attemptId: 'smoke-attempt-3' },
    );
  if (resumed.response.thread.id !== threadId || resumed.correlation.threadId !== threadId) {
      throw new Error('thread/resume did not preserve the thread correlation');
    }
  } catch (error) {
    // An empty thread has no persisted rollout until its first turn. Exercising and
    // decoding this request-aware error keeps the smoke free of model calls.
    if (!(error instanceof FactoryRemoteError) ||
      error.envelope.method !== 'thread/resume' ||
      !error.envelope.error.message.includes('no rollout found')) {
      throw error;
    }
    resumeOutcome = 'expected-no-rollout';
  }
  const threadStartedEvent = observedEvents.find(({ type }) => type === 'threadStarted');
  if (!threadStartedEvent || threadStartedEvent.attemptId !== seed.attemptId) {
    throw new Error('thread/started was not projected with its originating correlation');
  }

  const protocol = client.manifest.factoryProtocol.version;
  await client.close();
  client = undefined;
  console.log(JSON.stringify({
    ok: true,
    protocol,
    threadId,
    resumeOutcome,
    observedEvents,
    lifecycle: [
      'protocol-manifest',
      'initialize',
      'initialized',
      'thread/start',
      'thread/compact/start',
      'thread/resume',
      'eof',
    ],
  }));
} finally {
  if (client) await client.close().catch(() => undefined);
  await rm(temporaryRoot, { recursive: true, force: true });
}
