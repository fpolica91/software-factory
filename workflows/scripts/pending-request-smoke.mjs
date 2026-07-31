import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { access, mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { CoordinatorClient, CoordinatorHttpError } from '../dist/coordinator-client.js';
import { executeCodexOperation } from '../dist/lifecycle.js';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const repositoryRoot = resolve(packageRoot, '..');
const factorydPath = process.env.FACTORYD_BIN ?? resolve(
  repositoryRoot,
  'factory-harness',
  'factory',
  'target',
  'debug',
  'factoryd',
);
const fixtureRuntime = resolve(
  packageRoot,
  'scripts',
  'fixtures',
  'pending-request-runtime.mjs',
);
const postgresContainer = `factory-pending-request-${process.pid}`;
const temporaryRoot = await mkdtemp(resolve(tmpdir(), 'factory-pending-request-'));
const workspace = resolve(temporaryRoot, 'workspace');
const codexHome = resolve(temporaryRoot, 'codex-home');
await Promise.all([
  access(factorydPath),
  access(fixtureRuntime),
  mkdir(workspace, { recursive: true }),
  mkdir(codexHome, { recursive: true }),
]);

let factoryd;
let factorydStderr = '';
const lifecycleLog = [];

function docker(...args) {
  return execFileSync('docker', args, {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
}

async function waitFor(predicate, description, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await predicate();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 100));
  }
  throw new Error(
    `timed out waiting for ${description}${lastError ? `: ${String(lastError)}` : ''}`,
  );
}

async function request(baseUrl, method, path, body) {
  const response = await fetch(`${baseUrl}${path}`, {
    method,
    ...(body === undefined
      ? {}
      : {
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(body),
        }),
  });
  if (!response.ok) {
    throw new CoordinatorHttpError(
      response.status,
      await response.text(),
      `${method} ${path} failed with ${response.status}`,
    );
  }
  return response.status === 204 ? undefined : response.json();
}

async function startFactoryd(databaseUrl, bind = '127.0.0.1:0') {
  factorydStderr = '';
  const child = spawn(factorydPath, [
    '--database-url',
    databaseUrl,
    'serve',
    '--bind',
    bind,
  ], { stdio: ['ignore', 'pipe', 'pipe'] });
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => {
    factorydStderr += chunk;
  });
  child.stdout.setEncoding('utf8');
  let stdout = '';
  const listening = await new Promise((resolvePromise, reject) => {
    const onExit = (code, signal) => reject(new Error(
      `factoryd exited before readiness (${String(code)}/${String(signal)}): ${factorydStderr}`,
    ));
    child.once('exit', onExit);
    child.stdout.on('data', (chunk) => {
      stdout += chunk;
      const newline = stdout.indexOf('\n');
      if (newline < 0) return;
      child.off('exit', onExit);
      try {
        resolvePromise(JSON.parse(stdout.slice(0, newline)).listening);
      } catch (error) {
        reject(error);
      }
    });
  });
  factoryd = child;
  const baseUrl = `http://${listening}`;
  await waitFor(
    async () => (await fetch(`${baseUrl}/healthz`)).ok,
    'factoryd readiness',
  );
  return { baseUrl, bind: listening };
}

async function stopFactoryd() {
  if (!factoryd) return;
  const child = factoryd;
  factoryd = undefined;
  if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
  await new Promise((resolvePromise) => {
    if (child.exitCode !== null || child.signalCode !== null) resolvePromise();
    else child.once('close', resolvePromise);
  });
}

async function createClaim(baseUrl, kind, leaseSeconds) {
  const job = await request(baseUrl, 'POST', '/v1/jobs', {
    kind,
    input: {},
    operations: [{ kind: 'codex.execute', input: {}, maxAttempts: 1 }],
  });
  const operationId = job.operations[0].operationId;
  const lease = await request(
    baseUrl,
    'POST',
    `/v1/operations/${encodeURIComponent(operationId)}/claim`,
    { ownerInstanceId: `${kind}-worker`, leaseSeconds },
  );
  return { job, lease };
}

try {
  docker(
    'run', '--rm', '-d', '--name', postgresContainer,
    '-e', 'POSTGRES_PASSWORD=pending_request',
    '-e', 'POSTGRES_DB=pending_request',
    '-p', '127.0.0.1::5432',
    'postgres:16-alpine',
  );
  await waitFor(() => {
    try {
      docker('exec', postgresContainer, 'psql', '-U', 'postgres', '-d', 'pending_request', '-Atc', 'SELECT 1');
      return true;
    } catch {
      return false;
    }
  }, 'PostgreSQL readiness');
  const postgresPort = docker('port', postgresContainer, '5432/tcp').split(':').at(-1);
  const databaseUrl = `postgresql://postgres:pending_request@127.0.0.1:${postgresPort}/pending_request`;
  let server = await startFactoryd(databaseUrl);

  const { job, lease } = await createClaim(server.baseUrl, 'pending-lifecycle', 120);
  const coordinator = new CoordinatorClient(`${server.baseUrl}/v1`);
  const operationPromise = executeCodexOperation({
    coordinator,
    lease,
    kind: 'codex.execute',
    input: {
      cwd: workspace,
      prompt: 'Exercise the durable approval path.',
      runtimePath: fixtureRuntime,
      codexHome,
      model: 'fixture-model',
      modelProvider: 'fixture-provider',
      approvalPolicy: 'on-request',
      turnTimeoutSeconds: 30,
    },
    correlation: {
      jobId: lease.selection.jobId,
      operationId: lease.selection.operationId,
      attemptId: lease.attempt.attemptId,
    },
    log: async (message) => {
      lifecycleLog.push(message);
    },
  });

  const pending = await waitFor(async () => {
    const records = await request(
      server.baseUrl,
      'GET',
      `/v1/pending-requests?jobId=${encodeURIComponent(job.job.jobId)}`,
    );
    return records.length === 1 ? records[0] : undefined;
  }, 'durable command approval');
  assert.equal(pending.request.id, 700);
  assert.equal(pending.request.method, 'item/commandExecution/requestApproval');

  await stopFactoryd();
  server = await startFactoryd(databaseUrl, server.bind);
  const afterRestart = await request(
    server.baseUrl,
    'GET',
    `/v1/pending-requests/${encodeURIComponent(pending.pendingRequestId)}`,
  );
  assert.equal(afterRestart.state, 'pending');
  await request(
    server.baseUrl,
    'POST',
    `/v1/pending-requests/${encodeURIComponent(pending.pendingRequestId)}/resolve`,
    {
      response: {
        id: 700,
        method: 'item/commandExecution/requestApproval',
        response: { decision: 'accept' },
      },
    },
  );

  const result = await operationPromise;
  assert.equal(result.threadId, 'pending-request-thread');
  assert.equal(result.turnId, 'pending-request-turn');
  assert.equal(result.turn?.status, 'completed');
  await request(
    server.baseUrl,
    'POST',
    `/v1/attempts/${encodeURIComponent(lease.attempt.attemptId)}/complete`,
  );

  const inactive = await createClaim(server.baseUrl, 'pending-inactive', 1);
  const inactiveRecord = await request(server.baseUrl, 'POST', '/v1/pending-requests', {
    attemptId: inactive.lease.attempt.attemptId,
    request: {
      id: 'inactive-input',
      method: 'item/tool/requestUserInput',
      params: {
        threadId: 'inactive-thread',
        turnId: 'inactive-turn',
        itemId: 'inactive-item',
        questions: [],
      },
    },
  });
  await new Promise((resolvePromise) => setTimeout(resolvePromise, 1_200));
  const inactiveLoaded = await request(
    server.baseUrl,
    'GET',
    `/v1/pending-requests/${encodeURIComponent(inactiveRecord.pendingRequestId)}`,
  );
  assert.equal(inactiveLoaded.state, 'inactive');
  await assert.rejects(
    request(
      server.baseUrl,
      'POST',
      `/v1/pending-requests/${encodeURIComponent(inactiveRecord.pendingRequestId)}/resolve`,
      {
        response: {
          id: 'inactive-input',
          method: 'item/tool/requestUserInput',
          response: { answers: {} },
        },
      },
    ),
    (error) => error instanceof CoordinatorHttpError && error.status === 409,
  );

  console.log(JSON.stringify({
    phase: 'pendingRequestLifecycleAccepted',
    pendingRequestId: pending.pendingRequestId,
    factorydRestart: true,
    exactRequestId: pending.request.id,
    exactMethod: pending.request.method,
    nonHumanFallthrough: true,
    inboundReaderNonblocking: true,
    turnStatus: result.turn?.status,
    inactiveResolution: 'rejected',
    lifecycleLog,
  }));
} finally {
  await stopFactoryd().catch(() => undefined);
  try {
    docker('stop', postgresContainer);
  } catch {
    // The container may already be gone after a failed startup.
  }
  await rm(temporaryRoot, { recursive: true, force: true });
}
