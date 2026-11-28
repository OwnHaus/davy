// Backup and restore end to end. Backs up the running e2e server while it
// takes logs, to a file and to stdout, kills one backup halfway, verifies
// and restores, then starts a second server on the restored directory and
// asserts that it answers the same issues and logs as the first, up to the
// moment the backup started.
import assert from 'node:assert/strict'
import { execFileSync, spawn } from 'node:child_process'
import { closeSync, existsSync, openSync, readdirSync, readFileSync, statSync } from 'node:fs'
import { createServer } from 'node:net'
import { join } from 'node:path'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'

const [bin, data, url, token, scratch] = process.argv.slice(2)
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms))
const davy = (...args) => execFileSync(bin, args, { encoding: 'utf8' })

// Runs davy with its stdout into `out`, a file path, or kept as text.
function run(args, out) {
  return new Promise(resolve => {
    const fd = out === undefined ? 'pipe' : openSync(out, 'w', 0o600)
    const child = spawn(bin, args, { stdio: ['ignore', fd, 'pipe'] })
    let stdout = ''
    let stderr = ''
    child.stdout?.on('data', d => { stdout += d })
    child.stderr.on('data', d => { stderr += d })
    child.on('close', status => {
      if (fd !== 'pipe') closeSync(fd)
      resolve({ status, stdout, stderr })
    })
  })
}

async function mcp(at) {
  const transport = new StreamableHTTPClientTransport(new URL('/mcp', at), {
    requestInit: { headers: { Authorization: `Bearer ${token}` } },
  })
  const client = new Client({ name: 'davy-e2e-backup', version: '1.0.0' })
  await client.connect(transport)
  return client
}

async function call(client, name, args) {
  const result = await client.callTool({ name, arguments: args })
  assert.notEqual(result.isError, true, result.content[0].text)
  return JSON.parse(result.content[0].text)
}

const dsn = davy('project', 'create', 'backup', '--data', data, '--public-url', url).trim()
let sent = 0
async function send(n) {
  const logs = Array.from({ length: n }, (_, i) => ({ dt: new Date().toISOString(), level: 'info', message: `order ${sent + i} paid`, service: 'shop' }))
  const r = await fetch(new URL('/', url), {
    method: 'POST',
    headers: { Authorization: `Bearer ${new URL(dsn).username}`, 'Content-Type': 'application/json' },
    body: JSON.stringify(logs),
  })
  assert.equal(r.status, 202, await r.text())
  sent += n
}
const counted = dir => Number(davy('logs', '--project', 'backup', '--since', '1d', '--count', '--json', '--data', dir).match(/"total":(\d+)/)[1])
async function searchable(n) {
  const deadline = Date.now() + 20_000
  while (counted(data) < n) {
    assert.ok(Date.now() < deadline, `${n} logs were not searchable within 20 s`)
    await sleep(100)
  }
}

await send(50)
await searchable(50)
const before = new Date(Date.now() + 1).toISOString()
await sleep(5)

// A backup to a file while logs arrive.
let sending = true
const sender = (async () => { while (sending) { await send(20); await sleep(10) } })()
const file = join(scratch, 'backup.tar.zst')
const taken = await run(['backup', '--data', data, file])
sending = false
await sender
assert.equal(taken.status, 0, taken.stderr)
assert.match(taken.stdout, /^backed up \d+ files, .* the log writer paused for \d+ ms\.\nIt holds the secrets of this server/)
assert.equal(statSync(file).mode & 0o777, 0o600)
const record = JSON.parse(readFileSync(join(data, 'last-backup.json'), 'utf8'))
assert.equal(record.to, file)
await searchable(sent)
const paused = taken.stdout.match(/paused for (\d+) ms/)[1]
console.log(`backup: to a file while ${sent - 50} logs arrived; the log writer paused for ${paused} ms`)

// A backup killed halfway leaves the source as it was, and the next backup
// removes what it left.
const killed = spawn(bin, ['backup', '--data', data, '-'], { stdio: ['ignore', 'pipe', 'ignore'] })
await new Promise(resolve => killed.stdout.once('data', resolve))
killed.kill('SIGKILL')
await new Promise(resolve => killed.on('close', resolve))
await send(5)
await searchable(sent)
const streamed = join(scratch, 'stdout.tar.zst')
const piped = await run(['backup', '--data', data, '-'], streamed)
assert.equal(piped.status, 0, piped.stderr)
assert.match(piped.stderr, /to stdout as/)
const gate = join(data, 'logs', '.backup')
assert.deepEqual(readdirSync(gate).filter(n => n.startsWith('run-')), [])
assert.equal(counted(data), sent)
console.log('backup: a backup killed halfway left the source whole, and the next one, to stdout, removed its staging')

for (const backup of [file, streamed]) {
  const checked = await run(['backup', 'verify', backup])
  assert.equal(checked.status, 0, checked.stderr)
  assert.match(checked.stdout, /^the backup is whole: \d+ files, .* by Davy \S+ at schema version \d+/)
}
const cut = join(scratch, 'cut.tar.zst')
execFileSync('head', ['-c', String(statSync(file).size - 100), file], { stdio: ['ignore', openSync(cut, 'w'), 'inherit'] })
assert.notEqual((await run(['backup', 'verify', cut])).status, 0)
console.log('backup verify: both backups are whole, and one cut short fails')

// Restore, with its refusals.
const restored = join(scratch, 'restored')
const put = await run(['restore', '--data', restored, file])
assert.equal(put.status, 0, put.stderr)
assert.match(put.stdout, /^restored \d+ files/)
const again = await run(['restore', '--data', restored, file])
assert.notEqual(again.status, 0)
assert.match(again.stderr, /is not empty: restore into a new directory, or pass --replace/)
const live = await run(['restore', '--data', data, '--replace', file])
assert.notEqual(live.status, 0)
assert.match(live.stderr, /a Davy server or another restore runs on .*: stop it before you restore into it/)
const open = []
const walk = dir => {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name)
    const s = statSync(path)
    if (s.mode & 0o077) open.push(path)
    if (s.isDirectory()) walk(path)
  }
}
walk(restored)
assert.deepEqual(open, [])
assert.ok(!existsSync(join(restored, 'last-backup.json')))
console.log('restore: into a new directory, owner only; a non-empty one and a running server\'s are refused')

// A second server on the restored directory answers as the first did.
const port = await new Promise(resolve => {
  const s = createServer().listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => resolve(p)) })
})
const second = `http://127.0.0.1:${port}`
const server = spawn(bin, ['serve', '--data', restored, '--listen', `127.0.0.1:${port}`, '--public-url', second, '--no-self-report'], {
  env: { ...process.env, DAVY_TOKEN: token },
  stdio: ['ignore', 'ignore', 'pipe'],
})
let log = ''
server.stderr.on('data', d => { log += d })
try {
  const deadline = Date.now() + 20_000
  for (;;) {
    try { await fetch(second); break } catch { /* not up yet */ }
    assert.ok(Date.now() < deadline, `the restored server did not start: ${log}`)
    await sleep(100)
  }
  const [a, b] = [await mcp(url), await mcp(second)]
  const issues = 'SELECT i.id, p.slug, i.title, i.status, i.event_count, i.last_seen FROM issues i JOIN projects p ON p.id = i.project_id ORDER BY i.id'
  const [ia, ib] = [await call(a, 'sql', { query: issues }), await call(b, 'sql', { query: issues })]
  assert.ok(ia.rows.length > 5, JSON.stringify(ia))
  assert.deepEqual(ib, ia)
  const window = { project: 'backup', since: new Date(Date.now() - 86_400_000).toISOString(), until: before, limit: 1000 }
  const [la, lb] = [await call(a, 'search_logs', window), await call(b, 'search_logs', window)]
  assert.equal(la.length, 50)
  assert.deepEqual(lb, la)
  assert.deepEqual(await call(b, 'count_logs', window), await call(a, 'count_logs', window))
  // Of the logs sent during the backup, the restore holds those up to one
  // moment: 0..n, none missing in between. Logs the writer took but had not
  // committed at the pause are not in it, as after a crash, so it may hold
  // only the first 50.
  const all = davy('logs', '--project', 'backup', '--since', '1d', '--limit', '100000', '--json', '--data', restored)
  const numbers = all.trim().split('\n').map(l => Number(JSON.parse(l).message.split(' ')[1])).sort((x, y) => x - y)
  assert.deepEqual(numbers, numbers.map((_, i) => i))
  assert.ok(numbers.length >= 50 && numbers.length <= sent, numbers.length)
  assert.equal((await call(b, 'count_logs', { project: 'backup', since: '1d' })).total, numbers.length)
  await a.close()
  await b.close()
  console.log(`restore: a second server on the restored directory answers the same ${ia.rows.length} issues and 50 logs, and holds logs 0..${numbers.length - 1} of ${sent}`)
} finally {
  server.kill('SIGTERM')
  await new Promise(resolve => server.on('close', resolve))
}
