import type { IntegrationRegistry } from './registry.js';
import { installIntegration, type FactoryIntegrationPlugin } from './plugin.js';
import type { JsonValue } from './types.js';

export interface IntegrationPluginConfig {
  module: string;
  config?: Record<string, JsonValue>;
}

/** Load explicitly configured ESM adapter plugins into a factoryd-side registry. */
export async function loadIntegrations(
  registry: IntegrationRegistry,
  plugins: IntegrationPluginConfig[],
): Promise<void> {
  for (const entry of plugins) {
    if (!entry.module.trim()) throw new Error('integration plugin module must not be empty');
    const loaded = await import(entry.module) as Record<string, unknown>;
    const candidate = loaded.default ?? loaded.integration;
    if (!isPlugin(candidate)) {
      throw new Error(
        `integration module '${entry.module}' must export default or named 'integration' plugin`,
      );
    }
    await installIntegration(registry, candidate, entry.config ?? {});
  }
}

function isPlugin(value: unknown): value is FactoryIntegrationPlugin {
  return typeof value === 'object' && value !== null &&
    typeof (value as { name?: unknown }).name === 'string' &&
    typeof (value as { create?: unknown }).create === 'function';
}
