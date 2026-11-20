const [sdk, dsn] = process.argv.slice(2)
const Sentry = await import(sdk)
Sentry.init({ dsn, release: 'e2e@1', environment: 'test', tracesSampleRate: 0 })
function capture(Type) {
  try { throw new Type('SDK test failure') } catch (error) { Sentry.captureException(error) }
}
for (const Type of [TypeError, TypeError, RangeError]) capture(Type)
if (!await Sentry.flush(10000)) throw new Error('SDK flush timed out')
await Sentry.close(1000)
console.log(`${sdk}: sent TypeError twice and RangeError once`)
