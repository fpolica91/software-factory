import type { FactoryRuntimeManifest } from './distribution-manifest.js';
import type { FactoryErrorEnvelope } from './protocol/v1/generated/index.js';
import type { CodexRpcError } from './codex-v2/wire.js';

export class FactoryClientError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = 'FactoryClientError';
  }
}

export class FactoryProtocolCompatibilityError extends FactoryClientError {
  readonly expected: FactoryRuntimeManifest;
  readonly actual: FactoryRuntimeManifest;

  constructor(expected: FactoryRuntimeManifest, actual: FactoryRuntimeManifest) {
    super(
      `Factory runtime protocol mismatch: expected Factory ${formatFactory(expected)}, ` +
        `Codex V2 ${formatCodex(expected)}, revision ${expected.sourceCodexRevision}; received ` +
        `Factory ${formatFactory(actual)}, Codex V2 ${formatCodex(actual)}, ` +
        `revision ${actual.sourceCodexRevision}`,
    );
    this.name = 'FactoryProtocolCompatibilityError';
    this.expected = expected;
    this.actual = actual;
  }
}

function formatFactory(manifest: FactoryRuntimeManifest): string {
  const { version, schemaSha256 } = manifest.factoryProtocol;
  return `${version.major}.${version.minor}/${schemaSha256}`;
}

function formatCodex(manifest: FactoryRuntimeManifest): string {
  const { version, schemaSha256 } = manifest.codexAppServerV2;
  return `${version.major}/${schemaSha256}`;
}

export class FactoryRemoteError extends FactoryClientError {
  readonly envelope: FactoryErrorEnvelope;

  constructor(envelope: FactoryErrorEnvelope) {
    super(`Factory request ${envelope.method} failed (${envelope.error.code}): ${envelope.error.message}`);
    this.name = 'FactoryRemoteError';
    this.envelope = envelope;
  }
}

export class CodexRemoteError extends FactoryClientError {
  readonly requestId: string;
  readonly method: string;
  readonly error: CodexRpcError;

  constructor(requestId: string, method: string, error: CodexRpcError) {
    super(`Codex request ${method} failed (${error.code}): ${error.message}`);
    this.name = 'CodexRemoteError';
    this.requestId = requestId;
    this.method = method;
    this.error = error;
  }
}

export class FactoryProcessError extends FactoryClientError {
  readonly exitCode: number | null;
  readonly signal: NodeJS.Signals | null;
  readonly stderr: string;

  constructor(message: string, exitCode: number | null, signal: NodeJS.Signals | null, stderr: string) {
    super(message);
    this.name = 'FactoryProcessError';
    this.exitCode = exitCode;
    this.signal = signal;
    this.stderr = stderr;
  }
}
