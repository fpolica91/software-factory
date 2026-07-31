import { CODEX_V2_PROTOCOL_MANIFEST } from './codex-v2/generated/factoryManifest.js';
import {
  FACTORY_PROTOCOL_MANIFEST,
  type JsonValue,
} from './protocol/v1/generated/index.js';

export interface FactoryRuntimeManifest {
  factoryProtocol: {
    version: {
      major: number;
      minor: number;
    };
    schemaSha256: string;
  };
  sourceCodexRevision: string;
  codexAppServerV2: {
    version: {
      major: number;
    };
    schemaSha256: string;
  };
}

export const FACTORY_RUNTIME_MANIFEST = {
  factoryProtocol: {
    version: FACTORY_PROTOCOL_MANIFEST.version,
    schemaSha256: FACTORY_PROTOCOL_MANIFEST.schemaSha256,
  },
  sourceCodexRevision: CODEX_V2_PROTOCOL_MANIFEST.sourceCodexRevision,
  codexAppServerV2: {
    version: CODEX_V2_PROTOCOL_MANIFEST.version,
    schemaSha256: CODEX_V2_PROTOCOL_MANIFEST.schemaSha256,
  },
} as const satisfies FactoryRuntimeManifest;

export function decodeFactoryRuntimeManifest(value: JsonValue): FactoryRuntimeManifest {
  const manifest = record(value, 'Factory runtime manifest');
  const factoryProtocol = record(
    manifest.factoryProtocol,
    'Factory runtime manifest factoryProtocol',
  );
  const factoryVersion = record(
    factoryProtocol.version,
    'Factory runtime manifest factoryProtocol.version',
  );
  const codexAppServerV2 = record(
    manifest.codexAppServerV2,
    'Factory runtime manifest codexAppServerV2',
  );
  const codexVersion = record(
    codexAppServerV2.version,
    'Factory runtime manifest codexAppServerV2.version',
  );

  return {
    factoryProtocol: {
      version: {
        major: integer(
          factoryVersion.major,
          'Factory runtime manifest factoryProtocol.version.major',
        ),
        minor: integer(
          factoryVersion.minor,
          'Factory runtime manifest factoryProtocol.version.minor',
        ),
      },
      schemaSha256: string(
        factoryProtocol.schemaSha256,
        'Factory runtime manifest factoryProtocol.schemaSha256',
      ),
    },
    sourceCodexRevision: string(
      manifest.sourceCodexRevision,
      'Factory runtime manifest sourceCodexRevision',
    ),
    codexAppServerV2: {
      version: {
        major: integer(
          codexVersion.major,
          'Factory runtime manifest codexAppServerV2.version.major',
        ),
      },
      schemaSha256: string(
        codexAppServerV2.schemaSha256,
        'Factory runtime manifest codexAppServerV2.schemaSha256',
      ),
    },
  };
}

export function isPinnedFactoryRuntimeManifest(actual: FactoryRuntimeManifest): boolean {
  const factory = FACTORY_PROTOCOL_MANIFEST;
  const codex = CODEX_V2_PROTOCOL_MANIFEST;
  return actual.factoryProtocol.version.major === factory.version.major
    && actual.factoryProtocol.version.minor === factory.version.minor
    && actual.factoryProtocol.schemaSha256 === factory.schemaSha256
    && actual.sourceCodexRevision === factory.sourceCodexRevision
    && actual.sourceCodexRevision === codex.sourceCodexRevision
    && actual.codexAppServerV2.version.major === codex.version.major
    && actual.codexAppServerV2.schemaSha256 === codex.schemaSha256;
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new TypeError(`${label} must be an object`);
  }
  return value as Record<string, unknown>;
}

function integer(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0) {
    throw new TypeError(`${label} must be a non-negative integer`);
  }
  return value;
}

function string(value: unknown, label: string): string {
  if (typeof value !== 'string') throw new TypeError(`${label} must be a string`);
  return value;
}
