import type { FactoryExecutionReference } from './types.js';

export interface ArtifactReference {
  adapter: string;
  key: string;
  contentType?: string;
  size?: number;
}

export interface PutArtifactRequest {
  execution: FactoryExecutionReference;
  key: string;
  body: Uint8Array;
  contentType?: string;
}

/** Optional object-storage surface. MinIO is one deployment adapter, not a core dependency. */
export interface ArtifactAdapter {
  readonly name: string;

  put(request: PutArtifactRequest): Promise<ArtifactReference>;
  get(reference: ArtifactReference): Promise<Uint8Array>;
}
