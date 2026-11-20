import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'

const server = JSON.parse(readFileSync(process.argv[2], 'utf8')).mcpServers.davy
assert.equal(server.type, 'http')
const expand = value => value.replace(/\$\{(\w+)\}/g, (_, name) => {
  assert.ok(process.env[name], `${name} is not set`)
  return process.env[name]
})
const headers = Object.fromEntries(Object.entries(server.headers).map(([k, v]) => [k, expand(v)]))
const client = new Client({ name: 'davy-install-e2e', version: '1.0.0' })
await client.connect(new StreamableHTTPClientTransport(new URL(server.url), { requestInit: { headers } }))

async function call(name, args) {
  const result = await client.callTool({ name, arguments: args })
  assert.notEqual(result.isError, true, result.content[0].text)
  return JSON.parse(result.content[0].text)
}

const { rows } = await call('sql', {
  query: "SELECT i.id, i.title FROM issues i JOIN projects p ON p.id = i.project_id WHERE p.slug = 'my-app'",
})
assert.deepEqual(rows.map(r => r[1]), ['Error: Davy test error'])
const context = await call('issue_context', { issue_id: rows[0][0] })
assert.equal(context.latest_event.exceptions.at(-1).value, 'Davy test error')
console.log('verified: MCP from the .mcp.json that mcp-config printed returns the test error')
await client.close()
