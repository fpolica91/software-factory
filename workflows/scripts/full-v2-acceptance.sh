#!/usr/bin/env bash
set -euo pipefail

acceptance_token="$(date +%s)-$$-$RANDOM"
acceptance_root="$(mktemp -d "${TMPDIR:-/tmp}/factory-v2-complete.XXXXXX")"
acceptance_container="factory-v2-flow-postgres-${acceptance_token//[^a-zA-Z0-9]/}"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/../.." && pwd -P)"
node_pid=""

cleanup() {
  local status=$?
  trap - EXIT INT TERM
  if [[ -n "$node_pid" ]] && kill -0 "$node_pid" 2>/dev/null; then
    pkill -TERM -P "$node_pid" 2>/dev/null || true
    kill -TERM "$node_pid" 2>/dev/null || true
    wait "$node_pid" 2>/dev/null || true
  fi
  docker stop --time 1 "$acceptance_container" >/dev/null 2>&1 || true
  if [[ "$status" -ne 0 || "${FACTORY_KEEP_ACCEPTANCE_DIR:-0}" == "1" ]]; then
    printf 'preserved acceptance directory at %s\n' "$acceptance_root" >&2
  elif [[ -d "$acceptance_root" ]]; then
    if command -v gio >/dev/null 2>&1; then
      gio trash -- "$acceptance_root" || true
    else
      printf 'gio is unavailable; preserved acceptance directory at %s\n' "$acceptance_root" >&2
    fi
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

export FACTORY_ACCEPTANCE_ROOT="$acceptance_root"
export FACTORY_ACCEPTANCE_CONTAINER="$acceptance_container"
export FACTORY_REPO_ROOT="$repo_root"

node --input-type=module <<'FACTORY_ACCEPTANCE_NODE' &
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { spawn, spawnSync } from 'node:child_process';
import { access, copyFile, mkdir, mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const repoRoot = process.env.FACTORY_REPO_ROOT;
assert.ok(repoRoot, 'FACTORY_REPO_ROOT must identify the repository root');
const { CoordinatorClient } = await import(pathToFileURL(
  resolve(repoRoot, 'workflows/dist/coordinator-client.js'),
).href);
const { executeFactoryJob } = await import(pathToFileURL(
  resolve(repoRoot, 'workflows/dist/factory-job.js'),
).href);
const runtimePath = process.env.FACTORY_RUNTIME_PATH ??
  resolve(repoRoot, 'factory-harness/factory/target/debug/factory-runtime');
const factorydPath = process.env.FACTORYD_PATH ??
  resolve(repoRoot, 'factory-harness/factory/target/debug/factoryd');
const providerBaseUrl = process.env.FACTORY_PROVIDER_BASE_URL;
const customProvider = providerBaseUrl !== undefined && providerBaseUrl !== '';
const providerId = customProvider ? 'factory-provider' : 'openai';
const catalogPath = process.env.FACTORY_MODEL_CATALOG_JSON ??
  (customProvider
    ? '/tmp/software-factory-provider-glm52/codex-models.json'
    : resolve(repoRoot, 'factory-harness/codex-rs/models-manager/models.json'));
const model = process.env.FACTORY_MODEL ?? process.env.FACTORY_ZAI_MODEL ??
  (customProvider ? 'glm-5.2' : 'gpt-5.6-terra');
const userHome = process.env.HOME;
assert.ok(userHome, 'HOME must be set when using the canonical OpenAI provider');
const seedCodexHome = process.env.FACTORY_SEED_CODEX_HOME ?? resolve(userHome, '.codex');
const postgresImage = process.env.FACTORY_POSTGRES_IMAGE ?? 'postgres:16-alpine';
const token = randomUUID().replaceAll('-', '');
const containerName = process.env.FACTORY_ACCEPTANCE_CONTAINER ??
  `factory-v2-flow-postgres-${token.slice(0, 12)}`;
const temporaryRoot = process.env.FACTORY_ACCEPTANCE_ROOT ??
  await mkdtemp(resolve(tmpdir(), 'factory-v2-complete-'));
const origin = resolve(temporaryRoot, 'origin');
const workspaceRoot = resolve(temporaryRoot, 'workspaces');
const codexHome = resolve(temporaryRoot, 'codex-home');
let factoryd;
let ownsPostgres = false;

await Promise.all([
  access(runtimePath),
  access(factorydPath),
  access(catalogPath),
  ...(customProvider ? [] : [access(resolve(seedCodexHome, 'auth.json'))]),
]);

try {
  await Promise.all([
    mkdir(origin, { recursive: true }),
    mkdir(workspaceRoot, { recursive: true }),
    mkdir(codexHome, { recursive: true }),
  ]);
  run('git', ['init', '--initial-branch', 'main'], origin);
  run('git', ['config', 'user.name', 'Factory Acceptance'], origin);
  run('git', ['config', 'user.email', 'factory-acceptance@example.invalid'], origin);
  await writeFile(resolve(origin, 'README.md'), '# Factory V2 complete-flow fixture\n');
  run('git', ['add', 'README.md'], origin);
  run('git', ['commit', '-m', 'fixture: initialize'], origin);
  if (!customProvider) {
    await copyFile(resolve(seedCodexHome, 'auth.json'), resolve(codexHome, 'auth.json'));
  }
  await writeFile(resolve(codexHome, 'config.toml'), [
    `model = ${JSON.stringify(model)}`,
    `model_provider = ${JSON.stringify(providerId)}`,
    'model_reasoning_effort = "medium"',
    'approval_policy = "never"',
    'sandbox_mode = "danger-full-access"',
    `[projects.${JSON.stringify(workspaceRoot)}]`,
    'trust_level = "trusted"',
    '',
  ].join('\n'), { mode: 0o600 });

  docker([
    'run', '--detach', '--rm', '--name', containerName,
    '--env', 'POSTGRES_PASSWORD=factory',
    '--env', 'POSTGRES_DB=factory',
    '--publish', '127.0.0.1::5432',
    postgresImage,
  ]);
  ownsPostgres = true;
  const portOutput = docker(['port', containerName, '5432/tcp']).trim();
  const postgresPort = portOutput.match(/:(\d+)$/)?.[1];
  assert.ok(postgresPort, `could not resolve PostgreSQL port from ${portOutput}`);
  await waitForPostgres(containerName);
  const databaseUrl = `postgres://postgres:factory@127.0.0.1:${postgresPort}/factory`;
  run(factorydPath, ['--database-url', databaseUrl, 'migrate']);
  factoryd = await startFactoryd(databaseUrl, workspaceRoot);
  await waitForHttp(new URL('/healthz', factoryd.baseUrl), 'factoryd');
  if (customProvider) {
    await waitForHttp(
      new URL('models', providerBaseUrl.endsWith('/') ? providerBaseUrl : `${providerBaseUrl}/`),
      'provider',
    );
  }
  const factorydUrl = new URL('v1', factoryd.baseUrl).toString().replace(/\/$/, '');
  const coordinator = new CoordinatorClient(factorydUrl);

  const commonInput = {
    workspace: { repository: origin, baseRef: 'main' },
    runtimePath,
    codexHome,
    model,
    modelProvider: providerId,
    approvalPolicy: 'never',
    sandbox: 'danger-full-access',
    turnTimeoutSeconds: 900,
    env: { FACTORYD_URL: factorydUrl },
    config: customProvider
      ? {
          model_catalog_json: catalogPath,
          'model_providers.factory-provider': {
            name: 'Software Factory provider bridge',
            base_url: providerBaseUrl,
            wire_api: 'responses',
            requires_openai_auth: false,
            supports_websockets: false,
          },
        }
      : { model_catalog_json: catalogPath },
  };
  const created = await coordinator.createJob({
    kind: 'softwareFactory.completeV2Acceptance',
    workflowRunId: `factory-v2-complete-${token}`,
    input: commonInput,
    operations: [
      {
        kind: 'codex.plan',
        maxAttempts: 1,
        input: {
          prompt: 'Perform the native planning state transition, not merely a prose answer. Call factory_decompose exactly once with one unit: id proof-fixture; title Build reviewed proof fixture; description Create EXECUTED initially, have review require a REMEDIATED_OK marker, then remediate that marker; depends_on empty. Do not edit files. After the tool succeeds, finish with PLAN_ACCEPTED.',
        },
      },
      {
        kind: 'codex.execute',
        maxAttempts: 1,
        input: {
          prompt: 'Execute the initial fixture deliberately without the remediation marker. Use shell tools to create factory-v2-proof.txt containing exactly one line, EXECUTED, then read it back and verify REMEDIATED_OK is absent. Call factory_update_progress for unit_id proof-fixture with status completed and a summary that the initial fixture is ready for review. Finish with EXECUTE_ACCEPTED.',
        },
      },
      {
        kind: 'codex.review',
        maxAttempts: 1,
        input: {
          prompt: 'Inspect the actual uncommitted factory-v2-proof.txt without editing it. Confirm it contains EXECUTED but lacks the required REMEDIATED_OK marker. Call factory_record_review exactly once with verdict request_changes, a concise summary, and exactly one finding: id MISSING-REMEDIATED-MARKER; severity major; unit_id proof-fixture; title Missing remediation marker; evidence that the file lacks REMEDIATED_OK; recommendation to append REMEDIATED_OK as the second line. Finish with the review result.',
        },
      },
      {
        kind: 'codex.remediate',
        maxAttempts: 1,
        input: {
          prompt: 'Remediate the current durable review. Inspect factory-v2-proof.txt, append REMEDIATED_OK as exactly the second line, and verify the file now contains exactly EXECUTED then REMEDIATED_OK. Call factory_record_remediation exactly once with one disposition: finding_id MISSING-REMEDIATED-MARKER; disposition resolved; rationale that the required second-line marker was appended and verified; unit_id proof-fixture. Finish with REMEDIATION_ACCEPTED.',
        },
      },
    ],
  });

  const logs = [];
  const result = await executeFactoryJob({ jobId: created.job.jobId }, {
    ownerInstanceId: `acceptance-${token}`,
    workflowRunId: `factory-v2-complete-${token}`,
    log: async (message) => {
      logs.push(message);
      process.stderr.write(`${message}\n`);
    },
    sleepUntil: async (wakeAt) => {
      await delay(Math.max(0, wakeAt.getTime() - Date.now()));
    },
  }, coordinator);

  assert.equal(result.state, 'succeeded');
  assert.deepEqual(result.stages.map((stage) => stage.kind), [
    'codex.plan', 'codex.execute', 'codex.review', 'codex.remediate',
  ]);
  const threadIds = new Set(result.stages.map((stage) => stage.threadId));
  assert.equal(threadIds.size, 1, 'all stages must retain one root thread lineage');
  const threadId = result.stages[0].threadId;
  const durable = await coordinator.getThreadState(threadId);
  assert.equal(durable.revision, 4);
  assert.equal(durable.state.decomposition?.work_units?.[0]?.id, 'proof-fixture');
  assert.equal(durable.state.progress?.work_units?.[0]?.status, 'completed');
  assert.equal(durable.state.review?.verdict, 'request_changes');
  assert.equal(durable.state.review?.findings?.[0]?.id, 'MISSING-REMEDIATED-MARKER');
  assert.equal(durable.state.remediation?.records?.[0]?.disposition, 'resolved');

  const workspace = await coordinator.loadWorkspace(created.job.jobId);
  assert.equal(await readFile(resolve(workspace.root, 'factory-v2-proof.txt'), 'utf8'), 'EXECUTED\nREMEDIATED_OK\n');
  const rollouts = await loadRollouts(resolve(codexHome, 'sessions'));
  const reviewRollout = rollouts.find(({ records }) => {
    const meta = records.find((record) => record.type === 'session_meta')?.payload;
    return meta?.source?.subagent === 'review' && meta.parent_thread_id === threadId;
  });
  assert.ok(reviewRollout, 'native review child rollout with exact parent identity was not found');
  const reviewCalls = reviewRollout.records
    .filter((record) => record.type === 'response_item')
    .flatMap((record) => {
      if (record.payload?.type === 'function_call') return [record.payload.name];
      if (
        record.payload?.type !== 'custom_tool_call' ||
        record.payload.name !== 'exec' ||
        typeof record.payload.input !== 'string'
      ) return [];
      return [...record.payload.input.matchAll(/tools\.([A-Za-z0-9_]+)\s*\(/g)]
        .map((match) => match[1]);
    });
  assert.equal(reviewCalls.filter((name) => name === 'factory_record_review').length, 1);
  assert.equal(reviewCalls.filter((name) => name === 'factory_decompose').length, 0);
  const finalJob = await coordinator.loadJob(created.job.jobId);
  assert.equal(finalJob.job.state, 'succeeded');

  process.stdout.write(`${JSON.stringify({
    ok: true,
    temporaryRoot,
    jobId: created.job.jobId,
    providerId,
    providerBaseUrl: providerBaseUrl ?? null,
    model,
    threadId,
    stageTurnIds: result.stages.map((stage) => stage.turnId),
    factoryStateRevision: durable.revision,
    reviewParentThreadId: reviewRollout.records.find((record) => record.type === 'session_meta').payload.parent_thread_id,
    reviewCalls,
    proofFile: 'EXECUTED\\nREMEDIATED_OK\\n',
    operationalCheckpointWrites: logs.filter((line) => line.startsWith('checkpointed ')).length,
  })}\n`);
} catch (error) {
  process.stderr.write(`acceptance failed; preserving disposable evidence at ${temporaryRoot}\n`);
  throw error;
} finally {
  if (factoryd) await stopProcess(factoryd.child);
  if (ownsPostgres) docker(['stop', '--time', '1', containerName], true);
}

function run(command, args, cwd) {
  const result = spawnSync(command, args, { cwd, encoding: 'utf8' });
  if (result.status !== 0) throw new Error(`${command} failed: ${(result.stderr || result.stdout).trim()}`);
  return result.stdout ?? '';
}

function docker(args, allowFailure = false) {
  const result = spawnSync('docker', args, { encoding: 'utf8' });
  if (!allowFailure && result.status !== 0) throw new Error(`docker ${args[0]} failed: ${(result.stderr || result.stdout).trim()}`);
  return result.stdout ?? '';
}

async function waitForPostgres(name) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    const result = spawnSync('docker', ['exec', name, 'pg_isready', '--username', 'postgres', '--dbname', 'factory'], { encoding: 'utf8' });
    if (result.status === 0) return;
    await delay(250);
  }
  throw new Error('timed out waiting for disposable PostgreSQL');
}

async function startFactoryd(databaseUrl, managedWorkspaceRoot) {
  const child = spawn(factorydPath, ['--database-url', databaseUrl, 'serve', '--bind', '127.0.0.1:0'], {
    env: { ...process.env, FACTORY_WORKSPACE_ROOT: managedWorkspaceRoot },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '';
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr += chunk.toString(); });
  return new Promise((resolvePromise, rejectPromise) => {
    const deadline = setTimeout(() => rejectPromise(new Error(`timed out starting factoryd: ${stderr}`)), 30_000);
    child.stdout.on('data', (chunk) => {
      stdout += chunk.toString();
      const newline = stdout.indexOf('\n');
      if (newline < 0) return;
      try {
        const receipt = JSON.parse(stdout.slice(0, newline));
        clearTimeout(deadline);
        resolvePromise({ child, baseUrl: `http://${receipt.listening}/` });
      } catch (error) {
        clearTimeout(deadline);
        rejectPromise(new Error(`invalid factoryd receipt: ${stdout}\n${stderr}`, { cause: error }));
      }
    });
    child.once('exit', (code, signal) => {
      clearTimeout(deadline);
      rejectPromise(new Error(`factoryd exited during startup (${code ?? signal}): ${stderr}`));
    });
  });
}

async function stopProcess(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  child.kill('SIGTERM');
  await Promise.race([once(child, 'exit'), delay(5_000)]);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill('SIGKILL');
    await once(child, 'exit');
  }
}

async function waitForHttp(url, description) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {}
    await delay(250);
  }
  throw new Error(`timed out waiting for ${description} at ${url}`);
}

async function loadRollouts(root) {
  const files = [];
  async function visit(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) await visit(path);
      else if (entry.isFile() && entry.name.endsWith('.jsonl')) files.push(path);
    }
  }
  await visit(root);
  return Promise.all(files.map(async (path) => ({
    path,
    records: (await readFile(path, 'utf8')).split('\n').filter(Boolean).map((line) => JSON.parse(line)),
  })));
}

function delay(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}
FACTORY_ACCEPTANCE_NODE
node_pid=$!
wait "$node_pid"
node_pid=""
