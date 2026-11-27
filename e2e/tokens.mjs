// Agent tokens end to end: davy token add gives each agent a token of its
// own, the running server lists and refuses tools by its level, and only a
// token a person unlocked makes a drop rule live.
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'

const [bin, data, url] = process.argv.slice(2)
const davy = (...args) => execFileSync(bin, [...args, '--data', data], { encoding: 'utf8' })
const json = (...args) => JSON.parse(davy(...args))

execFileSync(bin, ['project', 'create', 'tokens', '--data', data, '--public-url', url], { encoding: 'utf8' })
const reader = json('token', 'add', 'e2e-reader', '--level', 'read')
assert.match(reader.secret, /^davy_agent_[A-Za-z0-9_-]{43}$/)
assert.deepEqual([reader.token.name, reader.token.level, reader.token.destructive], ['e2e-reader', 'read', false])
const ops = json('token', 'add', 'e2e-ops', '--level', 'full')
assert.equal(json('token', 'unlock', 'e2e-ops').token.destructive, true)
const listed = json('token', 'list', '--json')
assert.equal(JSON.stringify(listed).includes(reader.secret), false)

async function connect(secret) {
  const transport = new StreamableHTTPClientTransport(new URL('/mcp', url), {
    requestInit: { headers: { Authorization: `Bearer ${secret}` } },
  })
  const client = new Client({ name: 'davy-e2e-tokens', version: '1.0.0' })
  await client.connect(transport)
  const call = async (name, args) => {
    const result = await client.callTool({ name, arguments: args })
    return { text: result.content[0].text, isError: result.isError === true }
  }
  return { client, call }
}

// A read token sees no tool that changes anything, and a call to one is
// refused with the level it needs.
const read = await connect(reader.secret)
const names = (await read.client.listTools()).tools.map(t => t.name)
assert.equal(names.includes('mark_fixed'), false, names.join())
assert.equal(names.includes('drop_rule_live'), false, names.join())
const denied = await read.call('mark_fixed', { issue_id: 1 })
assert.equal(denied.isError, true, denied.text)
assert.match(denied.text, /^mark_fixed needs level triage, and the token "e2e-reader" holds read\./)

// The unlocked full token makes a shadow rule live. The message matches no
// line the rest of the run sends.
const full = await connect(ops.secret)
assert.equal((await full.client.listTools()).tools.some(t => t.name === 'drop_rule_live'), true)
const shadow = JSON.parse((await full.call('drop_rule_shadow', { project: 'tokens', message: 'e2e tokens never sent' })).text).rule
const live = await full.call('drop_rule_live', { project: 'tokens', rule_id: shadow.id })
assert.equal(live.isError, false, live.text)
const rule = JSON.parse(live.text).rule
assert.deepEqual([rule.id, rule.state, rule.by], [shadow.id, 'live', 'person'])

await read.client.close()
await full.client.close()
console.log('tokens: a read token is refused mark_fixed, an unlocked full token made a drop rule live')
