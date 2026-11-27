// The cold tier end to end. `before` sends logs for a day three days back
// and a few for now to a server that keeps 7 days hot, and saves what
// davy logs, search_logs and count_logs answer over a fixed window. run.sh
// then restarts the server with --hot-days 1, and `after` waits until that
// day is cold, then asserts the same rows and counts, with the part of the
// window that was cold named.
import assert from 'node:assert/strict'
import { execFileSync, spawnSync } from 'node:child_process'
import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'

const [phase, bin, data, url, token, saved] = process.argv.slice(2)
const davy = (...args) => execFileSync(bin, [...args, '--data', data], { encoding: 'utf8' })
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms))
const DAY = 86_400_000

async function mcp() {
  const transport = new StreamableHTTPClientTransport(new URL('/mcp', url), {
    requestInit: { headers: { Authorization: `Bearer ${token}` } },
  })
  const client = new Client({ name: 'davy-e2e-cold', version: '1.0.0' })
  await client.connect(transport)
  return client
}

// What the CLI and MCP answer over the window, with what each said of the
// cold tier set apart.
async function answers(client, w) {
  const at = ['--project', 'cold', '--since', w.since, '--until', w.until]
  const cli = spawnSync(bin, ['logs', ...at, '--limit', '1000', '--json', '--data', data], { encoding: 'utf8' })
  assert.equal(cli.status, 0, cli.stderr)
  const logs = cli.stdout.trim().split('\n').map(l => JSON.parse(l))
  const { cold: cliCold, ...cliCount } = JSON.parse(davy('logs', ...at, '--count', '--json'))
  const out = { logs, stderr: cli.stderr, cliCount, searches: [], counts: [], cold: [cliCold ?? null] }
  for (const filter of [{}, { query: 'payment AND timed' }, { level: 'error' }, { query: 'checkout', level: 'warn' }]) {
    const args = { project: 'cold', since: w.since, until: w.until, limit: 1000, ...filter }
    const found = await client.callTool({ name: 'search_logs', arguments: args })
    assert.notEqual(found.isError, true, found.content[0].text)
    out.searches.push(JSON.parse(found.content[0].text))
    out.cold.push(found.content[1] ? JSON.parse(found.content[1].text).cold : null)
    const counted = await client.callTool({ name: 'count_logs', arguments: args })
    const { cold, ...count } = JSON.parse(counted.content[0].text)
    out.counts.push(count)
    out.cold.push(cold ?? null)
  }
  return out
}

async function before() {
  const dsn = execFileSync(bin, ['project', 'create', 'cold', '--data', data, '--public-url', url], { encoding: 'utf8' }).trim()
  const now = Date.now()
  const day = new Date(Math.floor(now / DAY) * DAY - 3 * DAY).toISOString().slice(0, 10)
  const old = Array.from({ length: 60 }, (_, i) => ({
    dt: new Date(Date.parse(`${day}T10:00:00Z`) + i * 60_000).toISOString(),
    level: ['info', 'info', 'warn', 'error'][i % 4],
    message: i % 4 === 3 ? `payment provider timed out after ${1000 + i} ms` : `checkout ${i} placed for cart c-${i % 7}`,
    service: 'shop',
    route: `/api/orders/${i}`,
  }))
  const recent = [0, 1, 2].map(i => ({ dt: new Date(now - (3 - i) * 1000).toISOString(), level: 'error', message: `payment provider timed out after ${i} ms`, service: 'shop' }))
  const sent = await fetch(new URL('/', url), {
    method: 'POST',
    headers: { Authorization: `Bearer ${new URL(dsn).username}`, 'Content-Type': 'application/json' },
    body: JSON.stringify([...old, ...recent]),
  })
  assert.equal(sent.status, 202, await sent.text())
  // A whole window, fixed, so that the histogram's buckets stay the same.
  const w = { day, since: new Date(Date.parse(`${day}T00:00:00Z`) - DAY).toISOString(), until: new Date(now + 60_000).toISOString() }
  const deadline = Date.now() + 20_000
  while (davy('logs', '--project', 'cold', '--since', w.since, '--until', w.until, '--count', '--json').match(/"total":(\d+)/)?.[1] !== '63') {
    assert.ok(Date.now() < deadline, 'the 63 logs were not searchable within 20 s')
    await sleep(100)
  }
  const client = await mcp()
  const hot = await answers(client, w)
  await client.close()
  assert.equal(hot.logs.length, 63)
  assert.deepEqual(hot.searches.map(s => s.length), [63, 18, 18, 15])
  assert.deepEqual(hot.counts.map(c => c.total), [63, 18, 18, 15])
  assert.deepEqual(hot.cold, Array(9).fill(null), 'no day is cold yet')
  assert.equal(hot.stderr, '')
  writeFileSync(saved, JSON.stringify({ w, hot }))
  console.log(`cold tier: 63 logs searchable hot, 60 of them on ${day}`)
}

async function after() {
  const { w, hot } = JSON.parse(readFileSync(saved, 'utf8'))
  const logs = join(data, 'logs')
  const cold = () => !existsSync(join(logs, w.day)) && existsSync(join(logs, 'cold', w.day, '1'))
  const deadline = Date.now() + 60_000
  while (!cold()) {
    assert.ok(Date.now() < deadline, `${w.day} did not turn cold within 60 s`)
    await sleep(100)
  }
  const client = await mcp()
  const now = await answers(client, w)
  await client.close()
  assert.deepEqual(now.logs, hot.logs)
  assert.deepEqual(now.cliCount, hot.cliCount)
  assert.deepEqual(now.searches, hot.searches)
  assert.deepEqual(now.counts, hot.counts)
  assert.match(now.stderr, new RegExp(`^cold logs from ${w.day}T00:00:00Z to ${w.day}T23:59:59.999999999Z: 60 rows read in \\d+ ms\n$`))
  // Every answer read the cold day whole and nothing more, and named it.
  for (const note of now.cold) {
    assert.ok(note, 'an answer did not say it read a cold day')
    assert.ok(note.cold_from.startsWith(`${w.day}T00:00:00`) && note.cold_to.startsWith(`${w.day}T23:59:59`), JSON.stringify(note))
    assert.equal(note.unscanned_to, undefined, JSON.stringify(note))
    assert.equal(note.corrupt_blocks, undefined, JSON.stringify(note))
  }
  // Newest first: three logs fill a search from today, so no cold file opens.
  const client2 = await mcp()
  const newest = await client2.callTool({ name: 'search_logs', arguments: { project: 'cold', since: w.since, until: w.until, limit: 3 } })
  await client2.close()
  assert.equal(newest.content.length, 1, JSON.stringify(newest.content))
  assert.deepEqual(JSON.parse(newest.content[0].text).map(l => l.message), hot.logs.slice(-3).map(l => l.message))
  console.log(`cold tier: ${w.day} turned cold with --hot-days 1, and davy logs, search_logs and count_logs give the same 63 rows and counts`)
}

await (phase === 'before' ? before() : after())
