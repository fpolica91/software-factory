import { isAbsolute } from 'node:path';

import type { JsonValue } from '@software-factory/harness-client';
import { TerminalOperationError } from './lifecycle.js';

const PROVIDER_API_KEY_ENV = 'FACTORY_PROVIDER_API_KEY';
const PROVIDER_BRIDGE_TOKEN_ENV = 'FACTORY_PROVIDER_BRIDGE_TOKEN';
const SAFE_PROVIDER_ID = /^[A-Za-z0-9][A-Za-z0-9_-]*$/;

type ProviderAuthentication = 'key' | 'none' | 'bridge';

function configuredValue(
  environment: NodeJS.ProcessEnv,
  name: string,
): string | undefined {
  const value = environment[name]?.trim();
  return value ? value : undefined;
}

function providerAuthentication(environment: NodeJS.ProcessEnv): ProviderAuthentication {
  const value = (environment.FACTORY_PROVIDER_AUTH ?? 'key').trim().toLowerCase();
  if (value === 'key' || value === 'none' || value === 'bridge') return value;
  throw new TerminalOperationError(
    'FACTORY_PROVIDER_AUTH must be one of: key, none, bridge',
  );
}

function authenticationConfig(
  authentication: ProviderAuthentication,
  environment: NodeJS.ProcessEnv,
): Record<string, JsonValue> {
  if (authentication === 'none') return {};
  if (authentication === 'key') {
    if (!configuredValue(environment, PROVIDER_API_KEY_ENV)) {
      throw new TerminalOperationError(
        'FACTORY_PROVIDER_API_KEY is required when FACTORY_PROVIDER_AUTH=key',
      );
    }
    return { env_key: PROVIDER_API_KEY_ENV };
  }
  if (!configuredValue(environment, PROVIDER_BRIDGE_TOKEN_ENV)) {
    throw new TerminalOperationError(
      'FACTORY_PROVIDER_BRIDGE_TOKEN is required when FACTORY_PROVIDER_AUTH=bridge',
    );
  }
  return {
    env_http_headers: {
      'X-OpenCodex-API-Key': PROVIDER_BRIDGE_TOKEN_ENV,
    },
  };
}

function providerConfig(environment: NodeJS.ProcessEnv): Record<string, JsonValue> {
  const authentication = providerAuthentication(environment);
  const providerId = configuredValue(environment, 'FACTORY_MODEL_PROVIDER');
  const providerBaseUrl = configuredValue(environment, 'FACTORY_PROVIDER_BASE_URL');
  if (Boolean(providerId) !== Boolean(providerBaseUrl)) {
    throw new TerminalOperationError(
      'FACTORY_MODEL_PROVIDER and FACTORY_PROVIDER_BASE_URL must be configured together',
    );
  }
  if (!providerId || !providerBaseUrl) return {};
  if (!SAFE_PROVIDER_ID.test(providerId)) {
    throw new TerminalOperationError(
      'FACTORY_MODEL_PROVIDER must start with an alphanumeric character and contain only alphanumeric characters, hyphens, or underscores',
    );
  }

  return {
    [`model_providers.${providerId}`]: {
      name: configuredValue(environment, 'FACTORY_PROVIDER_NAME') ??
        'Software Factory deployment provider',
      base_url: providerBaseUrl,
      wire_api: 'responses',
      requires_openai_auth: false,
      supports_websockets: false,
      ...authenticationConfig(authentication, environment),
    },
  };
}

function modelCatalogRuntimeArgs(environment: NodeJS.ProcessEnv): string[] | undefined {
  const catalogPath = configuredValue(environment, 'FACTORY_MODEL_CATALOG_JSON');
  if (!catalogPath) return undefined;
  if (!isAbsolute(catalogPath)) {
    throw new TerminalOperationError('FACTORY_MODEL_CATALOG_JSON must be an absolute path');
  }
  return ['--config', `model_catalog_json=${JSON.stringify(catalogPath)}`];
}

export function deploymentCodexDefaults(
  environment: NodeJS.ProcessEnv = process.env,
): Record<string, JsonValue> {
  const defaults: Record<string, JsonValue> = {};
  const environmentFields = {
    runtimePath: configuredValue(environment, 'FACTORY_RUNTIME_PATH'),
    codexHome: configuredValue(environment, 'FACTORY_CODEX_HOME'),
    model: configuredValue(environment, 'FACTORY_MODEL'),
    modelProvider: configuredValue(environment, 'FACTORY_MODEL_PROVIDER'),
  };
  for (const [field, value] of Object.entries(environmentFields)) {
    if (value) defaults[field] = value;
  }

  const runtimeArgs = modelCatalogRuntimeArgs(environment);
  if (runtimeArgs) defaults.runtimeArgs = runtimeArgs;
  const config = providerConfig(environment);
  if (Object.keys(config).length > 0) defaults.config = config;
  return defaults;
}
