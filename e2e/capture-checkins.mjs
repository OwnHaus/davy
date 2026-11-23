// Captures the check-in envelopes `@sentry/node` sends, as fixtures for the
// parser tests: `node e2e/capture-checkins.mjs sentry-node-10`.
import http from 'node:http'
import fs from 'node:fs'
import zlib from 'node:zlib'
import os from 'node:os'

const sdk = process.argv[2]
const out = new URL(`./fixtures/${sdk}-checkins/`, import.meta.url)
fs.rmSync(out, { recursive: true, force: true })
fs.mkdirSync(out, { recursive: true })
let n = 0
const server = http.createServer((req, res) => {
  const chunks = []
  req.on('data', (c) => chunks.push(c))
  req.on('end', () => {
    let body = Buffer.concat(chunks)
    if (req.headers['content-encoding'] === 'gzip') body = zlib.gunzipSync(body)
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end('{"id":"0"}')
    // Client reports and sessions are captured with the other fixtures.
    if (!body.includes('{"type":"check_in"}')) return
    if (body.includes(os.homedir()) || body.includes(os.hostname()) || body.includes(os.userInfo().username)) {
      console.error('captured body contains a local path, host or user name', body.toString().slice(0, 3000))
      process.exit(1)
    }
    n += 1
    const headers = { ...req.headers }
    delete headers['content-encoding']
    fs.writeFileSync(new URL(`${n}.meta.json`, out), JSON.stringify({ method: req.method, url: req.url, headers }, null, 2))
    fs.writeFileSync(new URL(`${n}.body`, out), body)
  })
})

server.listen(0, async () => {
  const port = server.address().port
  const Sentry = await import(sdk)
  Sentry.init({ dsn: `http://tok123@127.0.0.1:${port}/1`, release: 'jobs@1.2.0', environment: 'production', serverName: 'dev-host', tracesSampleRate: 0 })
  const crontab = { schedule: { type: 'crontab', value: '0 * * * *' }, checkinMargin: 2, maxRuntime: 10, timezone: 'Europe/Prague' }
  await Sentry.withMonitor('nightly-report', async () => {}, crontab)
  await Sentry.flush(3000)
  try {
    await Sentry.withMonitor('import-feed', async () => { throw new Error('feed down') }, { schedule: { type: 'interval', value: 2, unit: 'hour' } })
  } catch {}
  await Sentry.flush(3000)
  const id = Sentry.captureCheckIn({ monitorSlug: 'billing-sync', status: 'in_progress' }, { schedule: { type: 'interval', value: 30, unit: 'minute' }, maxRuntime: 5 })
  await Sentry.flush(3000)
  Sentry.captureCheckIn({ checkInId: id, monitorSlug: 'billing-sync', status: 'ok', duration: 12.5 })
  await Sentry.flush(3000)
  Sentry.captureCheckIn({ monitorSlug: 'billing-sync', status: 'error' })
  await Sentry.flush(3000)
  server.close()
  console.log(sdk, 'captured', n)
})
