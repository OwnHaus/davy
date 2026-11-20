import http from 'node:http'
import fs from 'node:fs'
import zlib from 'node:zlib'
import os from 'node:os'

const name = process.argv[2]
const out = new URL(`./fixtures/${name}/`, import.meta.url)
fs.rmSync(out, { recursive: true, force: true })
fs.mkdirSync(out, { recursive: true })
let n = 0
const server = http.createServer((req, res) => {
  const chunks = []
  req.on('data', (c) => chunks.push(c))
  req.on('end', () => {
    let body = Buffer.concat(chunks)
    if (req.headers['content-encoding'] === 'gzip') body = zlib.gunzipSync(body)
    if (body.includes(os.homedir()) || body.includes(os.hostname()) || body.includes(os.userInfo().username)) {
      console.error('captured body contains a local path, host or user name', body.toString().slice(0, 3000))
      process.exit(1)
    }
    n += 1
    const headers = { ...req.headers, authorization: req.headers.authorization ? 'Bearer <token>' : undefined }
    delete headers['content-encoding']
    fs.writeFileSync(new URL(`${n}.meta.json`, out), JSON.stringify({ method: req.method, url: req.url, headers }, null, 2))
    fs.writeFileSync(new URL(`${n}.body`, out), body)
    const otlp = req.url === '/v1/logs'
    res.writeHead(200, { 'content-type': otlp ? req.headers['content-type'] : 'application/json' })
    res.end(otlp ? (req.headers['content-type'] === 'application/json' ? '{}' : '') : '{"id":"0"}')
  })
})

const traceId = '5b8efff798038103d269b633813fc60c'
const spanId = 'eee19b7ec3c1b174'

async function sentry(port) {
  const Sentry = await import('sentry-node-10')
  Sentry.init({ dsn: `http://tok123@127.0.0.1:${port}/1`, release: 'shop@2.4.0', environment: 'test', enableLogs: true, tracesSampleRate: 0, serverName: 'dev-host' })
  Sentry.logger.info('Order placed', { 'order.id': 'ord-1001', amount: 42.5, items: 3 })
  Sentry.logger.warn(Sentry.logger.fmt`Cart ${'c-77'} is over the limit`)
  Sentry.logger.error('Payment provider timed out', { provider: 'acme-pay', retry: true })
  await Sentry.flush(5000)
}

async function otlp(port, json) {
  const { LoggerProvider, SimpleLogRecordProcessor } = await import('@opentelemetry/sdk-logs')
  const { resourceFromAttributes } = await import('@opentelemetry/resources')
  const api = await import('@opentelemetry/api')
  const { OTLPLogExporter } = await import(json ? '@opentelemetry/exporter-logs-otlp-http' : '@opentelemetry/exporter-logs-otlp-proto')
  const exporter = new OTLPLogExporter({ url: `http://127.0.0.1:${port}/v1/logs`, headers: { Authorization: 'Bearer tok123' } })
  const provider = new LoggerProvider({
    resource: resourceFromAttributes({ 'service.name': 'checkout', 'deployment.environment': 'test' }),
    processors: [new SimpleLogRecordProcessor({ exporter })],
  })
  const logger = provider.getLogger('checkout.http', '1.0.0')
  const context = api.trace.setSpanContext(api.ROOT_CONTEXT, { traceId, spanId, traceFlags: 1 })
  logger.emit({ severityNumber: 9, severityText: 'INFO', body: 'GET /cart 200', attributes: { 'http.status_code': 200, 'user.id': 'u-17' }, context })
  logger.emit({ severityNumber: 17, severityText: 'ERROR', body: { error: 'db timeout', ms: 5000 }, attributes: { retry: false, tags: ['db', 'cart'] } })
  await provider.forceFlush()
  await provider.shutdown()
}

server.listen(0, async () => {
  const port = server.address().port
  if (name === 'sentry-node-10-logs') await sentry(port)
  else if (name === 'otlp-proto') await otlp(port, false)
  else if (name === 'otlp-json') await otlp(port, true)
  else throw new Error(`unknown fixture ${name}`)
  server.close()
  console.log(name, 'captured', n)
})
