import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { createInterface } from 'node:readline';

import { FactoryProcessError, FactoryProtocolCompatibilityError } from './errors.js';
import {
  decodeFactoryRuntimeManifest,
  FACTORY_RUNTIME_MANIFEST,
  isPinnedFactoryRuntimeManifest,
  type FactoryRuntimeManifest,
} from './distribution-manifest.js';
import { type JsonValue } from './protocol/v1/generated/index.js';
import { parseJsonlLine } from './protocol/v1/codec.js';

export interface FactoryProcessOptions {
  runtimePath: string;
  runtimeArgs?: readonly string[];
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  onStderr?: (chunk: string) => void;
}

export interface JsonlTransportCallbacks {
  onMessage(value: JsonValue): void | Promise<void>;
  onError(error: Error): void;
  onExit(code: number | null, signal: NodeJS.Signals | null, stderr: string): void;
}

function processEnvironment(overrides: NodeJS.ProcessEnv | undefined): NodeJS.ProcessEnv {
  return { ...process.env, ...overrides };
}

export async function negotiateProtocolManifest(
  options: FactoryProcessOptions,
): Promise<FactoryRuntimeManifest> {
  const { stdout, stderr, code, signal } = await runManifestCommand(options);
  if (code !== 0) {
    throw new FactoryProcessError(
      `factory-runtime protocol-manifest exited with code ${String(code)}`,
      code,
      signal,
      stderr,
    );
  }

  let actual: FactoryRuntimeManifest;
  try {
    actual = decodeFactoryRuntimeManifest(parseJsonlLine(stdout.trim()));
  } catch (error) {
    throw new FactoryProcessError(
      'factory-runtime protocol-manifest returned invalid JSON',
      code,
      signal,
      stderr,
    );
  }
  if (!isPinnedFactoryRuntimeManifest(actual)) {
    throw new FactoryProtocolCompatibilityError(FACTORY_RUNTIME_MANIFEST, actual);
  }
  return actual;
}

function runManifestCommand(options: FactoryProcessOptions): Promise<{
  stdout: string;
  stderr: string;
  code: number | null;
  signal: NodeJS.Signals | null;
}> {
  return new Promise((resolve, reject) => {
    const child = spawn(options.runtimePath, ['protocol-manifest'], {
      cwd: options.cwd,
      env: processEnvironment(options.env),
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk: string) => {
      stdout += chunk;
    });
    child.stderr.on('data', (chunk: string) => {
      stderr += chunk;
      options.onStderr?.(chunk);
    });
    child.once('error', reject);
    child.once('close', (code, signal) => resolve({ stdout, stderr, code, signal }));
  });
}

export class JsonlProcessTransport {
  readonly #child: ChildProcessWithoutNullStreams;
  readonly #callbacks: JsonlTransportCallbacks;
  #stderr = '';
  #writeTail = Promise.resolve();
  #inboundTail = Promise.resolve();

  private constructor(
    child: ChildProcessWithoutNullStreams,
    callbacks: JsonlTransportCallbacks,
    onStderr: FactoryProcessOptions['onStderr'],
  ) {
    this.#child = child;
    this.#callbacks = callbacks;
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    lines.on('line', (line) => {
      if (line.length === 0) return;
      this.#inboundTail = this.#inboundTail
        .then(async () => callbacks.onMessage(parseJsonlLine(line)))
        .catch((error: unknown) => callbacks.onError(asError(error)));
    });
    child.stderr.on('data', (chunk: string) => {
      this.#stderr += chunk;
      onStderr?.(chunk);
    });
    child.once('error', (error) => callbacks.onError(error));
    child.once('close', (code, signal) => {
      void this.#inboundTail.finally(() => callbacks.onExit(code, signal, this.#stderr));
    });
  }

  static start(options: FactoryProcessOptions, callbacks: JsonlTransportCallbacks): JsonlProcessTransport {
    const child = spawn(options.runtimePath, [...(options.runtimeArgs ?? [])], {
      cwd: options.cwd,
      env: processEnvironment(options.env),
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    return new JsonlProcessTransport(child, callbacks, options.onStderr);
  }

  send(value: JsonValue): Promise<void> {
    const line = `${JSON.stringify(value)}\n`;
    const write = this.#writeTail.then(() => new Promise<void>((resolve, reject) => {
      if (!this.#child.stdin.writable) {
        reject(new FactoryProcessError('factory-runtime stdin is closed', null, null, this.#stderr));
        return;
      }
      this.#child.stdin.write(line, 'utf8', (error) => {
        if (error) reject(error);
        else resolve();
      });
    }));
    this.#writeTail = write.catch(() => undefined);
    return write;
  }

  async closeInput(): Promise<void> {
    await this.#writeTail;
    if (!this.#child.stdin.writableEnded) this.#child.stdin.end();
  }
}

function asError(value: unknown): Error {
  return value instanceof Error ? value : new Error(String(value));
}
