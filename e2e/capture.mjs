import http from 'node:http'
import fs from 'node:fs'
import zlib from 'node:zlib'
import os from 'node:os'

const sdk = process.argv[2]
const out = new URL(`./fixtures/${sdk}/`, import.meta.url)
fs.mkdirSync(out, { recursive: true })
let n = 0
const server = http.createServer((req, res) => {
  const chunks = []
  req.on('data', (c) => chunks.push(c))
  req.on('end', () => {
    let body = Buffer.concat(chunks)
    const enc = req.headers['content-encoding']
    if (enc === 'gzip') body = zlib.gunzipSync(body)
    body = Buffer.from(body.toString('utf8').replaceAll(os.homedir(), '/home/dev').replaceAll(os.hostname(), 'dev-host'))
    n += 1
    fs.writeFileSync(new URL(`${n}.meta.json`, out), JSON.stringify({ method: req.method, url: req.url, headers: req.headers }, null, 2))
    fs.writeFileSync(new URL(`${n}.body`, out), body)
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end('{"id":"0"}')
  })
})
server.listen(0, async () => {
  const port = server.address().port
  const Sentry = await import(sdk)
  Sentry.init({ dsn: `http://abc123@127.0.0.1:${port}/1`, release: 'e2e@1.0.0', environment: 'test', tracesSampleRate: 1.0 })
  Sentry.setUser({ id: 'user-42' })
  try {
    JSON.parse('{nope')
  } catch (e) {
    Sentry.captureException(e)
  }
  Sentry.captureMessage('Plain message 123')
  await Sentry.flush(3000)
  server.close()
  console.log(sdk, 'captured', n)
})
