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
assert.deepEqual(tools.map(t => t.name).sort(), ['count_logs', 'issue_context', 'mark_fixed', 'schema', 'search_logs', 'sql', 'submit_feedback'])

async function call(name, args) {
  const result = await client.callTool({ name, arguments: args })
  return { text: result.content[0].text, isError: result.isError === true }
}

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
console.log(`mcp: verified 7 tools, sql, issue_context, search_logs, count_logs, mark_fixed on issue ${issue} and submit_feedback from client ${feedback.rows[0][0]}`)
