import type {
  ExternalReference,
  FactoryIntake,
  FactoryLifecycleEvent,
  IntegrationCursor,
  IntegrationPage,
  JsonValue,
} from './types.js';

export interface OperatorMessage {
  id: string;
  body: string;
  createdAt: string;
  parentId?: string;
}

export interface DecomposedWorkItem {
  localId: string;
  title: string;
  description: string;
  dependsOn: string[];
  metadata?: Record<string, JsonValue>;
}

/** Neutral tracker/intake surface used by factoryd-side workers. */
export interface IntakeAdapter {
  readonly name: string;

  listReady(cursor?: IntegrationCursor): Promise<IntegrationPage<ExternalReference>>;
  resolve(reference: ExternalReference): Promise<FactoryIntake>;
  listOperatorMessages(
    reference: ExternalReference,
    cursor?: IntegrationCursor,
  ): Promise<IntegrationPage<OperatorMessage>>;
  publishLifecycle(reference: ExternalReference, event: FactoryLifecycleEvent): Promise<void>;
  materializeDecomposition(
    reference: ExternalReference,
    work: DecomposedWorkItem[],
  ): Promise<ExternalReference[]>;
}
