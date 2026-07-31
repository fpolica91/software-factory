import { factoryJob } from './hatchet.js';

async function main(): Promise<void> {
  const jobId = process.argv[2];
  if (!jobId) {
    console.error('usage: npm run dispatch -- <job-id>');
    process.exit(2);
  }

  const run = await factoryJob.runNoWait({ jobId });
  const workflowRunId = await run.getWorkflowRunId();
  console.log(`dispatched factory-job workflow ${workflowRunId}`);
  const result = await run.output;
  console.log(JSON.stringify({ workflowRunId, result }, null, 2));
}

main().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
