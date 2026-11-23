// Sends cron check-ins the way a job instrumented with @sentry/node does.
// Usage: node e2e/send-checkins.mjs <sentry-node-7|sentry-node-10> <dsn>
const [sdk, dsn] = process.argv.slice(2)
const Sentry = await import(sdk)
Sentry.init({ dsn, release: 'jobs@1', environment: 'production', tracesSampleRate: 0 })

const hourly = {
  schedule: { type: 'crontab', value: '0 * * * *' },
  checkinMargin: 2,
  maxRuntime: 10,
  timezone: 'Europe/Prague',
}
// The SDK sends a run's start and end at once, so an instant job's end can
// arrive first. Davy answers after it has stored a check-in, so a job that
// flushes before it ends keeps them in order, as a job that runs a while does.
const job = (result) => async () => {
  if (!await Sentry.flush(5000)) throw new Error('SDK flush timed out')
  return result()
}
await Sentry.withMonitor('nightly-report', job(() => 'done'), hourly)
try {
  await Sentry.withMonitor('import-feed', job(() => { throw new Error('feed down') }), {
    schedule: { type: 'interval', value: 2, unit: 'hour' },
  })
} catch {}
const run = Sentry.captureCheckIn(
  { monitorSlug: 'billing-sync', status: 'in_progress' },
  { schedule: { type: 'interval', value: 30, unit: 'minute' }, maxRuntime: 5 },
)
await job(() => {})()
Sentry.captureCheckIn({ checkInId: run, monitorSlug: 'billing-sync', status: 'ok', duration: 12.5 })
// The operator made `backup` with `davy monitor add --slug`, so this config is ignored.
Sentry.captureCheckIn({ monitorSlug: 'backup', status: 'ok' }, { schedule: { type: 'crontab', value: '* * * * *' } })
// No monitor and no config: Davy counts it and drops it.
Sentry.captureCheckIn({ monitorSlug: 'typo-job', status: 'ok' })
if (!await Sentry.flush(10000)) throw new Error('SDK flush timed out')
await Sentry.close(1000)
console.log(`${sdk}: sent check-ins for nightly-report, import-feed, billing-sync, backup and typo-job`)
