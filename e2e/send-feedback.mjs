const [dsn] = process.argv.slice(2)
const Sentry = await import('sentry-node-10')
Sentry.init({
  dsn,
  release: 'e2e@1',
  tracesSampleRate: 0,
  beforeSend: (event) => (event.message === 'dropped by beforeSend' ? null : event),
})
let eventId
try { throw new TypeError('Checkout failed') } catch (error) { eventId = Sentry.captureException(error) }
Sentry.captureMessage('dropped by beforeSend')
if (!await Sentry.flush(10000)) throw new Error('SDK flush timed out')
Sentry.captureFeedback({
  message: 'The pay button does nothing',
  name: 'Ana',
  email: 'ana@example.com',
  url: 'https://shop.example.com/cart',
  associatedEventId: eventId,
})
if (!await Sentry.flush(10000)) throw new Error('SDK flush timed out')
await Sentry.close(1000)
console.log(`sentry-node-10: sent event ${eventId}, feedback about it, and one event beforeSend dropped`)
