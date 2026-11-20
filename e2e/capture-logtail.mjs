import http from 'node:http'
import fs from 'node:fs'
import pino from 'pino'

const out = new URL('./fixtures/logtail-pino/', import.meta.url)
fs.mkdirSync(out, { recursive: true })
let n = 0
const server = http.createServer((req, res) => {
  const chunks = []
  req.on('data', (c) => chunks.push(c))
  req.on('end', () => {
    n += 1
    fs.writeFileSync(new URL(`${n}.meta.json`, out), JSON.stringify({ method: req.method, url: req.url, headers: { ...req.headers, authorization: req.headers.authorization ? 'Bearer <token>' : undefined } }, null, 2))
    fs.writeFileSync(new URL(`${n}.body`, out), Buffer.concat(chunks))
    res.writeHead(202)
    res.end()
  })
})
server.listen(0, () => {
  const port = server.address().port
  const transport = pino.transport({ target: '@logtail/pino', options: { sourceToken: 'tok123', options: { endpoint: `http://127.0.0.1:${port}` } } })
  const log = pino({ level: 'debug', base: { pid: 4242, hostname: 'api-7d9f-x1k' } }, transport)
  log.info({ req: { id: 'r-1', method: 'GET', url: '/v1/events/1' }, res: { statusCode: 200 }, responseTime: 12 }, 'request completed')
  const err = new Error('connect ECONNREFUSED 10.0.1.5:5432')
  err.stack = 'Error: connect ECONNREFUSED 10.0.1.5:5432\n    at TCPConnectWrap.afterConnect [as oncomplete] (node:net:1611:16)'
  log.error({ err }, 'Unhandled exception')
  log.debug('debug line')
  setTimeout(() => transport.end(), 1500)
  transport.on('close', () => setTimeout(() => { server.close(); console.log('captured', n) }, 500))
})
