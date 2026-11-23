const [dsn] = process.argv.slice(2)
const http = await import('node:http')
const Sentry = await import('sentry-node-10')
Sentry.init({ dsn, release: 'e2e@2', environment: 'e2e', tracesSampleRate: 0 })

const flush = async () => {
  if (!await Sentry.flush(10000)) throw new Error('SDK flush timed out')
}

// What a browser or mobile SDK does: one session per user, updated on error.
// init already opened a session, and startSession sends it as exited. The
// flushes let each error update its session before the session ends.
Sentry.setUser({ id: 'ana' })
Sentry.startSession()
Sentry.captureSession()
Sentry.captureException(new Error('handled, so the session is errored'))
await flush()
Sentry.endSession()
Sentry.setUser({ id: 'bo' })
Sentry.startSession()
Sentry.captureSession()
Sentry.captureException(new Error('unhandled, so the session crashed'), {
  data: { mechanism: { type: 'e2e', handled: false } },
})
await flush()
Sentry.endSession()
Sentry.setUser(null)
await flush()

// What a server SDK does: count requests per minute, sent on flush.
const server = http.createServer((req, res) => {
  if (req.url === '/fail') Sentry.captureException(new Error('request failed'))
  res.end('ok')
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const { port } = server.address()
for (const path of ['/a', '/b', '/fail']) {
  const res = await fetch(`http://127.0.0.1:${port}${path}`)
  await res.text()
}
server.close()
await flush()
await Sentry.close(1000)
console.log('sentry-node-10: sent three user sessions (one errored, one crashed) and three request sessions (one errored)')
