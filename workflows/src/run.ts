import { hostname } from 'node:os';
import { CoordinatorClient } from './coordinator-client.js';
import { executeFactoryJob } from './factory-job.js';

const jobId = process.argv[2];
if (!jobId) {
  console.error('usage: npm run run -- <job-id>');
  process.exit(2);
}

executeFactoryJob({ jobId }, {
  ownerInstanceId: process.env.FACTORY_WORKER_INSTANCE_ID ?? `direct-${hostname()}-${process.pid}`,
  log: async (message) => {
    console.log(message);
  },
  sleepUntil: async (wakeAt) => {
    const delayMs = Math.max(0, wakeAt.getTime() - Date.now());
    await new Promise((resolve) => setTimeout(resolve, delayMs));
  },
}, new CoordinatorClient()).then((result) => {
  console.log(JSON.stringify(result, null, 2));
}).catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
