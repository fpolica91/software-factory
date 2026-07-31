import assert from 'node:assert/strict';
import { access, chmod, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  FactoryClient,
  FactoryProtocolCompatibilityError,
  FACTORY_RUNTIME_MANIFEST,
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

const temporaryRoot = await mkdtemp(resolve(tmpdir(), 'factory-manifest-handshake-'));
const codexHome = resolve(temporaryRoot, 'codex-home');
const workspace = resolve(temporaryRoot, 'workspace');
const alteredRuntime = resolve(temporaryRoot, 'altered-runtime.mjs');
const unexpectedInitializationMarker = resolve(temporaryRoot, 'unexpected-initialization');
await mkdir(codexHome, { recursive: true });
await mkdir(workspace, { recursive: true });

let client;
try {
  client = await FactoryClient.connect({
    runtimePath,
    cwd: workspace,
    env: { CODEX_HOME: codexHome },
  });
  assert.deepEqual(client.manifest, FACTORY_RUNTIME_MANIFEST);
  assert.equal(client.initializeResponse.codexHome, codexHome);
  await client.close();
  client = undefined;

  const wrapper = `#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';

const runtimePath = ${JSON.stringify(runtimePath)};
const markerPath = ${JSON.stringify(unexpectedInitializationMarker)};
const args = process.argv.slice(2);
if (args.length === 1 && args[0] === 'protocol-manifest') {
  const manifest = JSON.parse(execFileSync(runtimePath, args, { encoding: 'utf8' }).trim());
  manifest.codexAppServerV2.schemaSha256 = '0'.repeat(64);
  process.stdout.write(JSON.stringify(manifest) + '\\n');
} else {
  writeFileSync(markerPath, 'runtime lifecycle started\\n');
  process.exitCode = 97;
}
`;
  await writeFile(alteredRuntime, wrapper, { mode: 0o700 });
  await chmod(alteredRuntime, 0o700);

  let rejection;
  try {
    await FactoryClient.connect({
      runtimePath: alteredRuntime,
      cwd: workspace,
      env: { CODEX_HOME: codexHome },
    });
  } catch (error) {
    rejection = error;
  }
  assert.ok(rejection instanceof FactoryProtocolCompatibilityError);
  assert.equal(rejection.actual.codexAppServerV2.schemaSha256, '0'.repeat(64));
  assert.equal(
    rejection.expected.codexAppServerV2.schemaSha256,
    FACTORY_RUNTIME_MANIFEST.codexAppServerV2.schemaSha256,
  );
  await assert.rejects(access(unexpectedInitializationMarker), { code: 'ENOENT' });

  console.log(JSON.stringify({
    ok: true,
    factoryProtocol: FACTORY_RUNTIME_MANIFEST.factoryProtocol,
    sourceCodexRevision: FACTORY_RUNTIME_MANIFEST.sourceCodexRevision,
    codexAppServerV2: FACTORY_RUNTIME_MANIFEST.codexAppServerV2,
    alteredDigestRejectedBeforeInitialization: true,
  }));
} finally {
  if (client) await client.close().catch(() => undefined);
  await rm(temporaryRoot, { recursive: true, force: true });
}
