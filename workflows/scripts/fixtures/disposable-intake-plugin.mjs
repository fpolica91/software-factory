import { appendFile } from 'node:fs/promises';

function requiredString(value, label) {
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(`${label} must be a nonempty string`);
  }
  return value;
}

export default {
  name: 'disposable-intake-fixture',

  async create(config) {
    const outputPath = requiredString(config.outputPath, 'config.outputPath');
    const adapterName = requiredString(config.adapterName, 'config.adapterName');
    const failOnceEventId = typeof config.failOnceEventId === 'string'
      ? config.failOnceEventId
      : undefined;
    let failureDelivered = false;

    return {
      intake: {
        name: adapterName,

        async listReady() {
          return { items: [] };
        },

        async resolve(reference) {
          return {
            reference,
            title: reference.externalId,
            prompt: '',
            repository: { cloneUrl: 'https://repository.invalid/fixture.git' },
          };
        },

        async listOperatorMessages() {
          return { items: [] };
        },

        async publishLifecycle(reference, event) {
          const shouldFail = !failureDelivered && event.eventId === failOnceEventId;
          await appendFile(outputPath, `${JSON.stringify({
            outcome: shouldFail ? 'failed' : 'delivered',
            reference,
            event,
          })}\n`);
          if (shouldFail) {
            failureDelivered = true;
            throw new Error(`simulated lifecycle delivery failure for ${event.eventId}`);
          }
        },

        async materializeDecomposition() {
          return [];
        },
      },
    };
  },
};
