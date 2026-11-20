import { readFileSync } from 'node:fs'
import { Client } from '/opt/check/node_modules/@modelcontextprotocol/sdk/dist/esm/client/index.js'
import { StreamableHTTPClientTransport } from '/opt/check/node_modules/@modelcontextprotocol/sdk/dist/esm/client/streamableHttp.js'

const token = readFileSync('/var/lib/davy/token', 'utf8').trim()
const { public_url } = JSON.parse(readFileSync('/var/lib/davy/setup.json', 'utf8'))
const client = new Client({ name: 'davy-eval', version: '1.0.0' })
await client.connect(new StreamableHTTPClientTransport(new URL('mcp', public_url), {
  requestInit: { headers: { Authorization: `Bearer ${token}` } },
}))
const result = await client.callTool({ name: 'sql', arguments: { query: 'SELECT title FROM issues' } })
const titles = JSON.parse(result.content[0].text).rows.map(r => r[0])
console.log(JSON.stringify({ titles, test_error: titles.some(t => String(t).includes('Davy test error')) }))
await client.close()
