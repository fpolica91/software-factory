import type {
  ExternalReference,
  FactoryExecutionReference,
  IntegrationPage,
  JsonValue,
  RepositoryLocator,
} from './types.js';

export interface ChangeRequest {
  reference: ExternalReference;
  repository: RepositoryLocator;
  sourceRef: string;
  targetRef: string;
  revision: string;
  title: string;
  description?: string;
  state: 'open' | 'merged' | 'closed';
  draft: boolean;
}

export interface PublishChangeRequest {
  execution: FactoryExecutionReference;
  repository: RepositoryLocator;
  sourceRef: string;
  targetRef: string;
  title: string;
  description?: string;
  draft?: boolean;
  metadata?: Record<string, JsonValue>;
}

export interface ReviewThread {
  id: string;
  body: string;
  resolved: boolean;
  file?: string;
  line?: number;
}

export interface PipelineRun {
  id: string;
  revision: string;
  state: 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled';
  url?: string;
}

/** Neutral source-host surface; GitHub, GitLab, or another host can implement it. */
export interface SourceHostAdapter {
  readonly name: string;

  publishChange(request: PublishChangeRequest): Promise<ChangeRequest>;
  loadChange(reference: ExternalReference): Promise<ChangeRequest>;
  listReviewThreads(reference: ExternalReference): Promise<IntegrationPage<ReviewThread>>;
  replyToReview(reference: ExternalReference, threadId: string, body: string): Promise<void>;
  resolveReview(reference: ExternalReference, threadId: string): Promise<void>;
  loadPipeline(reference: ExternalReference, revision: string): Promise<PipelineRun | undefined>;
  triggerPipeline(reference: ExternalReference): Promise<PipelineRun>;
  loadPipelineFailure(reference: ExternalReference, pipelineId: string): Promise<string>;
}
