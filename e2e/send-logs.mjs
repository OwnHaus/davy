import pino from 'pino'

const [dsn] = process.argv.slice(2)
const url = new URL(dsn)
const key = url.username
const origin = `${url.protocol}//${url.host}`

async function logtail() {
  const transport = pino.transport({ target: '@logtail/pino', options: { sourceToken: key, options: { endpoint: origin } } })
  const log = pino({ level: 'debug', base: { pid: 4242, hostname: 'api-7d9f-x1k' } }, transport)
  log.info({ req: { method: 'GET', url: '/v1/events/1' }, res: { statusCode: 200 } }, 'logtail request completed')
  log.error({ err: { type: 'Error', message: 'connect ECONNREFUSED' } }, 'logtail unhandled exception')
  log.debug('logtail debug line')
  const alive = setInterval(() => {}, 1000)
  await new Promise((resolve) => {
    transport.on('close', resolve)
    setTimeout(() => transport.end(), 1500)
  })
  clearInterval(alive)
}

async function sentry() {
  const Sentry = await import('sentry-node-10')
  Sentry.init({ dsn, release: 'shop@2.4.0', environment: 'test', enableLogs: true, tracesSampleRate: 0, serverName: 'dev-host' })
  Sentry.logger.info('sentry order placed', { 'order.id': 'ord-1001', amount: 42.5 })
  Sentry.logger.warn(Sentry.logger.fmt`sentry cart ${'c-77'} is over the limit`)
  Sentry.logger.error('sentry payment provider timed out', { provider: 'acme-pay' })
  if (!await Sentry.flush(10000)) throw new Error('Sentry flush timed out')
  await Sentry.close(1000)
}

async function otlp(json) {
  const { LoggerProvider, SimpleLogRecordProcessor } = await import('@opentelemetry/sdk-logs')
  const { resourceFromAttributes } = await import('@opentelemetry/resources')
  const api = await import('@opentelemetry/api')
  const { OTLPLogExporter } = await import(json ? '@opentelemetry/exporter-logs-otlp-http' : '@opentelemetry/exporter-logs-otlp-proto')
  const exporter = new OTLPLogExporter({ url: `${origin}/v1/logs`, headers: { Authorization: `Bearer ${key}` } })
  const provider = new LoggerProvider({
    resource: resourceFromAttributes({ 'service.name': 'checkout', 'deployment.environment': 'test' }),
    processors: [new SimpleLogRecordProcessor({ exporter })],
  })
  const logger = provider.getLogger('checkout.http', '1.0.0')
  const name = json ? 'otlp json' : 'otlp proto'
  const context = api.trace.setSpanContext(api.ROOT_CONTEXT, { traceId: '5b8efff798038103d269b633813fc60c', spanId: 'eee19b7ec3c1b174', traceFlags: 1 })
  logger.emit({ severityNumber: 9, severityText: 'INFO', body: `${name} GET /cart 200`, attributes: { 'http.status_code': 200 }, context })
  logger.emit({ severityNumber: 17, severityText: 'ERROR', body: { error: `${name} db timeout`, ms: 5000 }, attributes: { retry: false } })
  await provider.forceFlush()
  await provider.shutdown()
}

await logtail()
await sentry()
await otlp(false)
await otlp(true)
console.log('logs: sent 3 via @logtail/pino, 3 via Sentry logger, 2 via OTLP proto, 2 via OTLP JSON')
