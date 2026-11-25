import assert from 'node:assert/strict'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'

const [url, token] = process.argv.slice(2)
const transport = new StreamableHTTPClientTransport(new URL('/mcp', url), {
  requestInit: { headers: { Authorization: `Bearer ${token}` } },
})
const client = new Client({ name: 'davy-e2e', version: '1.0.0' })
await client.connect(transport)

const { tools } = await client.listTools()
assert.deepEqual(tools.map(t => t.name).sort(), ['connect_project', 'count_logs', 'issue_context', 'log_patterns', 'mark_fixed', 'project_status', 'release_health', 'schema', 'search_logs', 'set_retention', 'sql', 'submit_feedback', 'usage'])

async function call(name, args) {
  const result = await client.callTool({ name, arguments: args })
  return { text: result.content[0].text, isError: result.isError === true }
}

const connected = JSON.parse((await call('connect_project', { slug: 'e2e-connect', sdk: 'node' })).text)
assert.equal(connected.project.slug, 'e2e-connect')
assert.equal(connected.sdk.package, '@sentry/node')
const missing = await call('connect_project', { slug: 'must-not-create' })
assert.equal(missing.isError, true)
let arrived
for (let i = 0; i < 100; i++) {
  arrived = JSON.parse((await call('project_status', { slug: 'e2e-connect' })).text)
  if (arrived.event_count === 1) break
  await new Promise(resolve => setTimeout(resolve, 100))
}
assert.equal(arrived.event_count, 1)
assert.equal(arrived.issue_count, 1)
assert.match(arrived.last_event_time, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d/)
assert.equal(arrived.last_log_time, null)
console.log('PASS connect: CLI test event confirmed by MCP project_status (1 event, 1 issue)')

assert.equal(arrived.retention.errors.days, 36500)
assert.equal(arrived.retention.errors.server_default, true)
const kept = JSON.parse((await call('set_retention', { project: 'e2e-connect', logs_days: 90, errors_days: 'default' })).text)
assert.deepEqual([kept.logs.days, kept.logs.server_default, kept.errors.days, kept.errors.server_default], [90, false, 36500, true])
const outOfRange = await call('set_retention', { project: 'e2e-connect', errors_days: 0 })
assert.deepEqual(outOfRange, { text: 'retention is 1 to 36500 days, not 0', isError: true })
const shorter = await call('set_retention', { project: 'e2e-connect', logs_days: 30 })
assert.equal(shorter.isError, true)
assert.match(shorter.text, /^an agent may only keep data longer: logs are kept 90 days and 30 would delete/)
const toDefault = await call('set_retention', { project: 'e2e-connect', logs_days: 'default' })
assert.equal(toDefault.isError, true)
assert.match(toDefault.text, /^an agent may not return logs to the server default: the project keeps them 90 days of its own/)
const stillThere = JSON.parse((await call('project_status', { slug: 'e2e-connect' })).text)
assert.equal(stillThere.event_count, 1)

// Ingest counts reach usage_daily on the server's next flush, every 5 s.
let used
for (let i = 0; i < 150; i++) {
  used = JSON.parse((await call('usage', { project: 'e2e-connect' })).text)
  if (used.projects[0].signals.errors.records === 1) break
  await new Promise(resolve => setTimeout(resolve, 100))
}
assert.equal(used.projects.length, 1)
assert.deepEqual([used.projects[0].project, used.projects[0].logs_days, used.projects[0].signals.errors.records], ['e2e-connect', 90, 1])
assert.match(used.disk.total, /^\d+(\.\d)? [KMGTP]?B$/)
assert.match(used.projects[0].signals.errors.bytes, /^\d+(\.\d)? [KMGTP]?B$/)
assert.equal(typeof used.projects[0].logs_stored.records, 'number')
// A server minutes old has counted raw log bytes for under an hour and
// holds no closed log day, so the forecast waits and has no basis.
assert.equal(used.basis, null)
assert.match(used.no_forecast, /^Davy has counted raw log bytes for \d+ minutes; a first forecast needs an hour of them, or closed log days on disk$/)
const whatIf = JSON.parse((await call('usage', { what_if: { logs_bytes_per_day: 1000000 } })).text)
assert.equal(whatIf.growth_per_day, '1.0 MB')
assert.equal(whatIf.basis, null)
console.log(`PASS usage: 1 error counted for e2e-connect, ${used.projects[0].signals.errors.bytes}, forecast: ${whatIf.forecast}`)

const schema = await call('schema', {})
assert.match(schema.text, /CREATE TABLE issues/)

const found = await call('sql', {
  query: "SELECT i.id, i.title FROM issues i JOIN projects p ON p.id = i.project_id WHERE p.slug = 'sentry-node-10' ORDER BY i.title DESC",
})
const rows = JSON.parse(found.text).rows
assert.deepEqual(rows.map(r => r[1]), ['TypeError: SDK test failure', 'RangeError: SDK test failure'])
const issue = rows[0][0]

const refused = await call('sql', { query: 'DELETE FROM issues' })
assert.equal(refused.isError, true, refused.text)

const context = JSON.parse((await call('issue_context', { issue_id: issue })).text)
assert.equal(context.issue.event_count, 2)
assert.equal(context.latest_event.release, 'e2e@1')
assert.equal(context.latest_event.exceptions.at(-1).type, 'TypeError')
assert.ok(context.latest_event.exceptions.at(-1).frames.length > 0)
assert.equal(context.recent_events.length, 2)
assert.deepEqual(context.releases.map(r => [r.release, r.events]), [['e2e@1', 2]])

const logs = JSON.parse((await call('search_logs', { query: 'timeout', project: 'logs', since: '10m', level: 'error' })).text)
assert.deepEqual(logs.map(l => l.message).sort(), ['{"error":"otlp json db timeout","ms":5000}', '{"error":"otlp proto db timeout","ms":5000}'])
const counts = JSON.parse((await call('count_logs', { query: 'timeout', project: 'logs', since: '10m', level: 'error' })).text)
assert.equal(counts.total, 2)
assert.equal(counts.buckets.reduce((sum, [, n]) => sum + n, 0), 2)
const patterns = JSON.parse((await call('log_patterns', { project: 'logs', since: '10m', level: 'error' })).text)
const otlpJson = patterns.top.find(p => p.example === '{"error":"otlp json db timeout","ms":5000}')
assert.deepEqual([otlpJson.template, otlpJson.records, otlpJson.levels], ['{"error":"otlp json db timeout", "ms":<NUM>}', 1, { error: 1 }])
const byPattern = JSON.parse((await call('search_logs', { project: 'logs', since: '10m', pattern: otlpJson.id })).text)
assert.deepEqual(byPattern.map(l => [l.message, l.pattern]), [['{"error":"otlp json db timeout","ms":5000}', otlpJson.id]])

const health = JSON.parse((await call('release_health', { project: 'health', since: '1d' })).text)
assert.deepEqual(health.map(r => [r.release, r.sessions, r.crashed, r.crash_free_sessions, r.users, r.crash_free_users]), [['e2e@2', 6, 1, 83.33, 2, 50]])

const fixed = await call('mark_fixed', { issue_id: issue, release: 'e2e@2' })
assert.deepEqual(fixed, { text: `issue ${issue} resolved in release e2e@2`, isError: false })
const status = JSON.parse((await call('sql', { query: `SELECT status, resolved_in_release FROM issues WHERE id = ${issue}` })).text)
assert.deepEqual(status.rows, [['resolved', 'e2e@2']])

const thanks = await call('submit_feedback', { tried: 'e2e tried', happened: 'e2e happened' })
assert.deepEqual(thanks, { text: 'Thanks. The feedback is stored for the person who runs Davy.', isError: false })
const feedback = JSON.parse((await call('sql', { query: "SELECT client, tried, happened, fix FROM feedback WHERE tried = 'e2e tried'" })).text)
assert.equal(feedback.rows.length, 1)
assert.deepEqual(feedback.rows[0].slice(1), ['e2e tried', 'e2e happened', null])
assert.ok(feedback.rows[0][0], 'the client name comes from _meta or User-Agent')

await client.close()
console.log(`mcp: verified 13 tools, set_retention, usage, sql, issue_context, search_logs, count_logs, log_patterns, release_health, mark_fixed on issue ${issue} and submit_feedback from client ${feedback.rows[0][0]}`)
