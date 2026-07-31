import { factoryJob, factoryJobFromTemplate, hatchet } from './hatchet.js';

async function main(): Promise<void> {
  const slots = Number(process.env.FACTORY_WORKFLOW_SLOTS ?? 4);
  const worker = await hatchet.worker('factory-workflows', {
    workflows: [factoryJob, factoryJobFromTemplate],
    slots,
  });
  console.log(`factory workflow worker starting with ${slots} slots`);
  await worker.start();
}

main().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
