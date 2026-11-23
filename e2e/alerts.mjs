import { execFile, execFileSync, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import { createHmac } from 'node:crypto'
import { createServer } from 'node:http'
import { createServer as createTcpServer } from 'node:net'
import { once } from 'node:events'
import path from 'node:path'
import assert from 'node:assert/strict'

const [bin, scratch] = process.argv.slice(2)
const data = path.join(scratch, 'data')
const e2e = path.dirname(new URL(import.meta.url).pathname)

async function listen(server) {
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  return server.address().port
}

let siteStatus = 503
const site = createServer((req, res) => res.writeHead(siteStatus).end('shop'))
const sitePort = await listen(site)

const hooks = []
const receiver = createServer((req, res) => {
  let body = ''
  req.on('data', chunk => { body += chunk })
  req.on('end', () => {
    hooks.push({ path: req.url, headers: req.headers, body })
    res.writeHead(204).end()
  })
})
const hookPort = await listen(receiver)

const mails = []
const smtp = createTcpServer(socket => {
  let buffer = ''
  let message = null
  socket.write('220 e2e\r\n')
  socket.on('data', chunk => {
    buffer += chunk
    let end
    while ((end = buffer.indexOf('\r\n')) >= 0) {
      const line = buffer.slice(0, end)
      buffer = buffer.slice(end + 2)
      if (message) {
        if (line === '.') {
          mails.push(message.join('\n'))
          message = null
          socket.write('250 queued\r\n')
        } else {
          message.push(line)
        }
      } else if (line === 'DATA') {
        message = []
        socket.write('354 go on\r\n')
      } else if (line === 'QUIT') {
        socket.end('221 bye\r\n')
      } else {
        socket.write('250 ok\r\n')
      }
    }
  })
  socket.on('error', () => {})
})
const smtpPort = await listen(smtp)

const probe = createServer()
const davyPort = await listen(probe)
probe.close()
const url = `http://127.0.0.1:${davyPort}`
const davy = (...args) => execFileSync(bin, [...args, '--data', data], { encoding: 'utf8' }).trim()

const dsn = execFileSync(bin, ['project', 'create', 'shop', '--data', data, '--public-url', url], { encoding: 'utf8' }).trim()
const [, secret] = davy('channel', 'add', 'webhook', `http://127.0.0.1:${hookPort}/hook`, '--all').split('\t')
davy('channel', 'add', 'slack', `http://127.0.0.1:${hookPort}/slack`, '--all')
davy('channel', 'add', 'email', 'ops@example.com', '--all')
davy('monitor', 'add', 'shop', 'API', '--url', `http://127.0.0.1:${sitePort}/`, '--every', '30s', '--timeout', '2s')
const [, pingPath] = davy('monitor', 'add', 'shop', 'cron', '--heartbeat', '--every', '1m', '--grace', '0s').split('\t')

const server = spawn(bin, ['serve', '--data', data, '--listen', `127.0.0.1:${davyPort}`, '--public-url', url, '--mail-from', 'Davy <alerts@davy.example>'], {
  env: { ...process.env, DAVY_SMTP_URL: `smtp://127.0.0.1:${smtpPort}` },
  stdio: 'inherit',
})
const started = Date.now()
const ping = async suffix => assert.equal((await fetch(`${url}${pingPath}${suffix}`, { method: 'POST' })).status, 200)
try {
  for (let i = 0; ; i++) {
    try { await fetch(`${url}/`); break } catch (error) { if (i > 100) throw error }
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  const sent = await promisify(execFile)('node', [path.join(e2e, 'send.mjs'), 'sentry-node-10', dsn])
  process.stdout.write(sent.stdout)
  await ping('')
  await ping('/fail')
  await ping('')

  const webhooks = () => hooks.filter(h => h.path === '/hook')
  const types = () => webhooks().map(h => JSON.parse(h.body)).map(b => `${b.type} ${b.monitor?.name ?? b.issues.map(i => i.issue.title).toSorted().join(', ')}`)
  const deadline = Date.now() + 120_000
  while (webhooks().length < 6 && Date.now() < deadline) {
    if (siteStatus === 503 && types().includes('monitor.down API')) siteStatus = 200
    await new Promise(resolve => setTimeout(resolve, 250))
  }
  await new Promise(resolve => setTimeout(resolve, 2000))

  assert.deepEqual(types().toSorted(), [
    'issue.digest RangeError: SDK test failure, TypeError: SDK test failure',
    'monitor.down API',
    'monitor.down cron',
    'monitor.down cron',
    'monitor.up API',
    'monitor.up cron',
  ])
  const ids = new Set()
  for (const hook of webhooks()) {
    const body = JSON.parse(hook.body)
    const [t, v1] = hook.headers['davy-signature'].split(',').map(part => part.split('=')[1])
    const expected = createHmac('sha256', secret).update(`${t}.${hook.body}`).digest('hex')
    assert.equal(v1, expected, 'webhook signature')
    assert.equal(hook.headers['davy-delivery'], String(body.id))
    assert.equal(body.project, 'shop')
    ids.add(body.id)
  }
  assert.equal(ids.size, 6, 'each delivery has its own id')
  const digest = webhooks().map(h => JSON.parse(h.body)).find(b => b.type === 'issue.digest')
  assert.deepEqual(digest.issues.map(i => [i.type, i.project, i.url]).toSorted(), digest.issues.map(i => ['issue.new', 'shop', `${url}/ui/issues/${i.issue.id}`]).toSorted())
  const order = name => types().filter(t => t.endsWith(` ${name}`)).map(t => t.split(' ')[0])
  assert.deepEqual(order('API'), ['monitor.down', 'monitor.up'])
  assert.deepEqual(order('cron'), ['monitor.down', 'monitor.up', 'monitor.down'])
  const reasons = webhooks().map(h => JSON.parse(h.body)).filter(b => b.type === 'monitor.down').map(b => `${b.monitor.name}: ${b.incident.reason}`)
  assert.deepEqual(reasons.toSorted(), ['API: HTTP 503', 'cron: failure ping', 'cron: no ping in time'])

  const lines = [
    '[shop] 2 new issues',
    '[shop] API is down: HTTP 503',
    '[shop] API is up again',
    '[shop] cron is down: failure ping',
    '[shop] cron is down: no ping in time',
    '[shop] cron is up again',
  ]
  const slack = hooks.filter(h => h.path === '/slack')
  assert.deepEqual(slack.map(h => JSON.parse(h.body).text.split('\n')[0]).toSorted(), lines)
  assert.ok(slack.every(h => h.headers['davy-signature'] === undefined), 'Slack gets no signature')

  const header = (mail, name) => mail.split('\n').find(l => l.startsWith(`${name}: `))?.slice(name.length + 2)
  assert.deepEqual(mails.map(m => header(m, 'Subject')).toSorted(), lines)
  assert.ok(mails.every(m => header(m, 'To') === 'ops@example.com' && header(m, 'From') === 'Davy <alerts@davy.example>'))
  assert.equal(new Set(mails.map(m => header(m, 'Message-ID'))).size, 6, 'each mail has its own Message-ID')
  assert.ok(mails.every(m => /^<davy\.\d+\.\d+@davy\.example>$/.test(header(m, 'Message-ID'))))

  const channels = JSON.parse(davy('channel', 'list', '--json'))
  assert.deepEqual(channels.map(c => [c.kind, c.unsent, c.failed, c.dropped]), [['webhook', 0, 0, 0], ['slack', 0, 0, 0], ['email', 0, 0, 0]])
  const seconds = Math.round((Date.now() - started) / 1000)
  console.log(`alerts: verified 6 signed webhooks, 6 Slack messages and 6 e-mails for a digest of 2 new issues, an HTTP monitor down and up, and a heartbeat failed, up and late, in ${seconds}s`)
} finally {
  server.kill()
  site.close()
  receiver.close()
  smtp.close()
}
