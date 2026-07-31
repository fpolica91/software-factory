import type { FactoryAdapters, IntegrationRegistry } from './registry.js';
import type { JsonValue } from './types.js';

export interface FactoryIntegrationPlugin {
  readonly name: string;
  create(config: Record<string, JsonValue>): FactoryAdapters | Promise<FactoryAdapters>;
}

/** Small authoring helper that preserves an adapter module's concrete types. */
export function defineIntegration<T extends FactoryIntegrationPlugin>(plugin: T): T {
  return plugin;
}

export async function installIntegration(
  registry: IntegrationRegistry,
  plugin: FactoryIntegrationPlugin,
  config: Record<string, JsonValue> = {},
): Promise<void> {
  if (!plugin.name.trim()) throw new Error('integration plugin name must not be empty');
  registry.register(await plugin.create(config));
}
