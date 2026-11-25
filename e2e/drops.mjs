// Drop rules end to end: a person adds live rules with davy drop, an agent
// adds a shadow rule over MCP and cannot make it live, nor stage a rule on
// Davy's own project or one of warnings, the running server drops and
// counts the lines they name, the savings view shows what the shadow rule
// would drop and what it would hide, and davy drop live asks before a rule
// goes live.
import assert from 'node:assert/strict'
import { execFileSync, spawnSync } from 'node:child_process'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'

const [bin, data, url, token] = process.argv.slice(2)
// As Davy quotes a word for a POSIX shell.
const sh = s => /^[A-Za-z0-9/._:@=+\-,%]+$/.test(s) ? s : `'${s.replaceAll("'", "'\\''")}'`
// Options first, then the slug after --.
const davy = (...args) => {
  const at = args.includes('--') ? args.indexOf('--') : args.length
  return execFileSync(bin, [...args.slice(0, at), '--data', data, ...args.slice(at)], { encoding: 'utf8' })
}
const json = (...args) => JSON.parse(davy(...args))
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms))

const dsn = execFileSync(bin, ['project', 'create', 'drops', '--data', data, '--public-url', url], { encoding: 'utf8' }).trim()
const key = new URL(dsn).username

// A person's live rules: debug and below dropped, half of the health checks
// kept. Naming two kinds of line in one rule is refused.
const debug = json('drop', 'add', '--level', 'debug', '--', 'drops')
assert.deepEqual([debug.state, debug.by, debug.level, debug.keep_percent, debug.created, debug.taken_over, debug.make_live], ['live', 'person', 'debug', 0, true, false, undefined])
const health = json('drop', 'add', '--message', 'health check', '--keep', '50%', '--', 'drops')
assert.deepEqual([health.state, health.message, health.keep_percent, health.position], ['live', 'health check', 50, 2])
const two = spawnSync(bin, ['drop', 'add', '--level', 'debug', '--message', 'x', '--data', data, '--', 'drops'], { encoding: 'utf8' })
assert.notEqual(two.status, 0)
assert.match(two.stderr, /name exactly one of pattern, level and message/)

// An agent's shadow rule. No field makes it live.
const transport = new StreamableHTTPClientTransport(new URL('/mcp', url), {
  requestInit: { headers: { Authorization: `Bearer ${token}` } },
})
const client = new Client({ name: 'davy-e2e-drops', version: '1.0.0' })
await client.connect(transport)
async function call(name, args) {
  const result = await client.callTool({ name, arguments: args })
  return { text: result.content[0].text, isError: result.isError === true }
}
const live = await call('drop_rule_shadow', { project: 'drops', message: 'poll tick', live: true })
assert.equal(live.isError, true, live.text)
const added = JSON.parse((await call('drop_rule_shadow', { project: 'drops', message: 'poll tick' })).text)
const shadow = added.rule
const makeLive = `a person can run: davy drop live --data ${sh(data)} ${shadow.id}`
assert.deepEqual([shadow.state, shadow.by, shadow.message, shadow.position, shadow.make_live], ['shadow', 'agent', 'poll tick', 3, makeLive])
assert.match(added.note, /^A shadow rule drops nothing/)
const refused = await call('drop_rule_remove', { project: 'drops', rule_id: debug.id })
assert.equal(refused.isError, true, refused.text)
// No rule of an agent on Davy's own project, whose warnings davy doctor
// reads, nor a level rule that names warnings.
const own = json('sql', 'SELECT slug FROM projects WHERE own=1', '--json').rows[0][0]
const onOwn = await call('drop_rule_shadow', { project: own, level: 'debug' })
assert.equal(onOwn.isError, true, onOwn.text)
assert.match(onOwn.text, /is Davy's own, whose warnings and errors davy doctor and project_status read: an agent stages no rule on it/)
const warn = await call('drop_rule_shadow', { project: 'drops', level: 'warn' })
assert.equal(warn.isError, true, warn.text)
assert.match(warn.text, /a level rule of warn names warnings and errors: an agent stages a level rule below warn only/)

// The server reads the rules every 5 s, then the lines go through them.
await sleep(6000)
const lines = [
  ...[1, 2, 3, 4].map(n => ({ message: `debug noise ${n}`, level: 'debug' })),
  ...[1, 2, 3, 4].map(() => ({ message: 'health check ok', level: 'info' })),
  ...[1, 2, 3, 4, 5, 6, 7, 8, 9, 10].map(n => ({ message: `poll tick ${n}`, level: 'info' })),
  { message: 'disk almost full', level: 'warn' },
]
const sent = await fetch(`${url}/`, {
  method: 'POST',
  headers: { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json' },
  body: JSON.stringify(lines),
})
assert.equal(sent.status, 202)

// The counts reach the store with the next usage flush, every 5 s.
let listed
for (let i = 0; i < 100; i++) {
  listed = json('drop', 'list', '--json', 'drops')
  if (listed.map(r => r.last_7_days.dropped.records + r.last_7_days.would_drop.records).join() === '4,2,10') break
  await sleep(200)
}
assert.deepEqual(listed.map(r => [r.id, r.state, r.last_7_days.dropped.records, r.last_7_days.would_drop.records]), [[debug.id, 'live', 4, 0], [health.id, 'live', 2, 0], [shadow.id, 'shadow', 0, 10]])
const wouldDrop = listed[2].last_7_days.would_drop.bytes
assert.ok(wouldDrop > 0, wouldDrop)
const text = davy('drop', 'list', 'drops')
assert.ok(text.includes(`shadow agent  message contains "poll tick", drop all: would drop 10 lines (`), text)
assert.ok(text.endsWith(`in 7 days, expires ${shadow.expires_at}; ${makeLive}\n`), text)

// What was stored: the shadow rule dropped nothing, the live rules 6 lines.
let stored = []
for (let i = 0; i < 100 && stored.length < 13; i++) {
  if (i) await sleep(200)
  stored = davy('logs', '--project', 'drops', '--since', '10m', '--json').split('\n').filter(Boolean).map(l => JSON.parse(l).message)
}
assert.deepEqual(stored.sort(), ['disk almost full', 'health check ok', 'health check ok', ...[1, 2, 3, 4, 5, 6, 7, 8, 9, 10].map(n => `poll tick ${n}`)].sort())
const [usage] = json('usage', '--project', 'drops', '--json').projects
assert.equal(usage.logs_filtered.records, 6)
assert.ok(usage.logs_filtered.bytes > 0, usage.logs_filtered)

// The savings view shows what the shadow rule would drop. A message rule
// can drop warnings too, so the view names no command but what it would
// hide, and no rate a day before it counted a full day. MCP gives sizes as
// text, the CLI bytes.
const view = JSON.parse((await call('usage', { view: 'savings', project: 'drops', top: 20 })).text)
const entry = view.savings.find(e => e.kind === 'shadow')
assert.deepEqual([entry.id, entry.project, entry.by, entry.message, entry.would_drop.records, entry.saves_per_day, entry.suggest, entry.hides], [shadow.id, 'drops', 'agent', 'poll tick', 10, null, null, 'every line that holds the text, warnings and errors included'])
assert.ok(entry.hours >= 0 && entry.hours < 1, entry.hours)
assert.match(entry.would_drop.bytes, /^\d+(\.\d)? [KMGTP]?B$/)
const cli = json('usage', '--project', 'drops', '--view', 'savings', '--json').savings.find(e => e.kind === 'shadow')
assert.equal(cli.would_drop.bytes, wouldDrop)
// The poll lines are info and nobody searched for them: keep 1%.
const poll = view.savings.find(e => e.kind === 'pattern' && e.example.startsWith('poll tick'))
assert.equal(poll.suggest, `keep 1%; a person can run: davy drop add --pattern ${poll.id} --keep 1% --data ${sh(data)} -- drops`)
assert.equal(poll.shadow, `drop_rule_shadow project="drops" pattern="${poll.id}" keep="1%"`)
assert.equal(poll.try, `search_logs pattern=${poll.id}`)
assert.equal(view.savings.find(e => e.kind === 'pattern' && e.example === 'disk almost full').suggest, null)

// The command the listing prints asks first, so without a terminal it
// refuses and the rule stays a shadow rule. With --yes the rule goes live
// and is the person's, and the agent can no longer remove it.
const printed = sh(bin) + shadow.make_live.split('a person can run: davy')[1]
const asked = spawnSync('sh', ['-c', printed], { encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] })
assert.notEqual(asked.status, 0)
assert.match(asked.stderr, new RegExp(`^rule ${shadow.id} of drops: message contains "poll tick", drop all\nin the last 7 days it would have dropped 10 lines \\(`))
assert.match(asked.stderr, /it can drop warnings and errors too\n/)
assert.match(asked.stderr, new RegExp(`there is no terminal to ask on: add --yes to make rule ${shadow.id} live`))
assert.equal(json('drop', 'list', '--json', 'drops')[2].state, 'shadow')
const turned = JSON.parse(execFileSync('sh', ['-c', `${printed} --yes`], { encoding: 'utf8' }))
assert.deepEqual([turned.id, turned.state, turned.by, turned.expires_at, turned.position], [shadow.id, 'live', 'person', null, 4])
assert.equal((await call('drop_rule_remove', { project: 'drops', rule_id: shadow.id })).isError, true)
const rules = JSON.parse((await call('drop_rules', { project: 'drops' })).text).rules
assert.deepEqual(rules.map(r => [r.id, r.state, r.by]), [[debug.id, 'live', 'person'], [health.id, 'live', 'person'], [shadow.id, 'live', 'person']])
// An agent removes a shadow rule of its own.
const mine = JSON.parse((await call('drop_rule_shadow', { project: 'drops', level: 'trace', keep: '10%' })).text).rule
assert.deepEqual(JSON.parse((await call('drop_rule_remove', { project: 'drops', rule_id: mine.id })).text), { removed: mine.id })
// A person who adds the same shadow rule as an agent takes it over: it is
// the person's then, never expires, and the agent cannot remove it.
const staged = JSON.parse((await call('drop_rule_shadow', { project: 'drops', level: 'info', keep: '10%' })).text).rule
const taken = json('drop', 'add', '--level', 'info', '--keep', '10%', '--shadow', '--', 'drops')
assert.deepEqual([taken.id, taken.state, taken.by, taken.expires_at, taken.created, taken.taken_over], [staged.id, 'shadow', 'person', null, false, true])
assert.equal((await call('drop_rule_remove', { project: 'drops', rule_id: staged.id })).isError, true)
assert.deepEqual(json('drop', 'remove', String(staged.id)), { removed: staged.id })
assert.deepEqual(json('drop', 'remove', String(health.id)), { removed: health.id })
await client.close()
console.log(`drops: live rules dropped 4 debug lines and 2 of 4 health checks (logs_filtered 6), agent shadow rule ${shadow.id} would drop 10 lines (${entry.would_drop.bytes}), the printed command asked and refused without a terminal, and a person made it live with --yes; agent rules on ${own} and of warn refused`)
