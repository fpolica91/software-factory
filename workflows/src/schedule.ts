import { CoordinatorClient } from './coordinator-client.js';

function usage(): never {
  console.error(
    'usage:\n' +
    '  npm run schedule -- at <ISO-timestamp> <job-id>\n' +
    '  npm run schedule -- cron <name> <cron-expression> <job-id>',
  );
  process.exit(2);
}

function isoTimestamp(value: string): Date {
  if (!/^\d{4}-\d{2}-\d{2}T/.test(value)) {
    throw new Error(`scheduled timestamp must be ISO 8601, got ${value}`);
  }
  const timestamp = new Date(value);
  if (Number.isNaN(timestamp.getTime())) {
    throw new Error(`scheduled timestamp is invalid: ${value}`);
  }
  return timestamp;
}

async function main(): Promise<void> {
  const [mode, ...args] = process.argv.slice(2);
  const coordinator = new CoordinatorClient();

  if (mode === 'at') {
    if (args.length !== 2) usage();
    const [timestampText, jobId] = args as [string, string];
    const triggerAt = isoTimestamp(timestampText);
    await coordinator.loadJob(jobId);
    const { factoryJob } = await import('./hatchet.js');
    const scheduled = await factoryJob.schedule(triggerAt, { jobId });
    console.log(JSON.stringify({
      kind: 'scheduled',
      scheduleId: scheduled.metadata.id,
      triggerAt: triggerAt.toISOString(),
      jobId,
    }, null, 2));
    return;
  }

  if (mode === 'cron') {
    if (args.length !== 3) usage();
    const [name, expression, jobId] = args as [string, string, string];
    if (!name.trim() || !expression.trim()) usage();
    await coordinator.loadJob(jobId);
    const { factoryJobFromTemplate } = await import('./hatchet.js');
    const cron = await factoryJobFromTemplate.cron(name, expression, {
      templateJobId: jobId,
    });
    console.log(JSON.stringify({
      kind: 'cron',
      cronId: cron.metadata.id,
      name,
      expression,
      templateJobId: jobId,
    }, null, 2));
    return;
  }

  usage();
}

main().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
