import type { ArtifactAdapter } from './artifacts.js';
import type { IntakeAdapter } from './intake.js';
import type { SourceHostAdapter } from './source-host.js';

export interface FactoryAdapters {
  intake?: IntakeAdapter;
  sourceHost?: SourceHostAdapter;
  artifacts?: ArtifactAdapter;
}

export class IntegrationRegistry {
  readonly #intake = new Map<string, IntakeAdapter>();
  readonly #sourceHosts = new Map<string, SourceHostAdapter>();
  readonly #artifacts = new Map<string, ArtifactAdapter>();

  register(adapters: FactoryAdapters): void {
    if (adapters.intake) this.#add(this.#intake, adapters.intake);
    if (adapters.sourceHost) this.#add(this.#sourceHosts, adapters.sourceHost);
    if (adapters.artifacts) this.#add(this.#artifacts, adapters.artifacts);
  }

  intake(name: string): IntakeAdapter {
    return this.#get(this.#intake, 'intake', name);
  }

  sourceHost(name: string): SourceHostAdapter {
    return this.#get(this.#sourceHosts, 'source-host', name);
  }

  artifacts(name: string): ArtifactAdapter {
    return this.#get(this.#artifacts, 'artifact', name);
  }

  #add<T extends { readonly name: string }>(registry: Map<string, T>, adapter: T): void {
    const name = adapter.name.trim();
    if (!name) throw new Error('integration adapter name must not be empty');
    if (registry.has(name)) throw new Error(`integration adapter '${name}' is already registered`);
    registry.set(name, adapter);
  }

  #get<T>(registry: Map<string, T>, kind: string, name: string): T {
    const adapter = registry.get(name);
    if (!adapter) throw new Error(`${kind} integration adapter '${name}' is not registered`);
    return adapter;
  }
}
