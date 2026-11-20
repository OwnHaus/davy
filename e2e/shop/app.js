import * as Sentry from 'sentry-node-10'

function parseOrder(raw) {
  const order = JSON.parse(raw)
  return order.items.length
}

export async function run(dsn) {
  Sentry.init({ dsn, release: 'shop@2.0.0', environment: 'test' })
  try {
    parseOrder('{"id": 7}')
  } catch (error) {
    Sentry.captureException(error)
  }
  await Sentry.flush(5000)
}
