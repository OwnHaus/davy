// Builds e2e/shop/app.js with esbuild, uploads its source map to Davy with
// sentry-cli, then runs the bundle so @sentry/node reports its error.
import fs from 'node:fs'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const here = fileURLToPath(new URL('.', import.meta.url))
const [work, url, token, dsn] = process.argv.slice(2)
const bin = `${here}node_modules/.bin/`
fs.mkdirSync(`${work}/src`, { recursive: true })
fs.copyFileSync(`${here}shop/app.js`, `${work}/src/app.js`)
const run = (tool, args, env = process.env) => execFileSync(`${bin}${tool}`, args, { cwd: work, env, stdio: ['ignore', 'ignore', 'inherit'] })
run('esbuild', ['src/app.js', '--bundle', '--minify', '--sourcemap', '--platform=node', '--format=esm', '--external:sentry-node-10', '--outfile=dist/app.min.js', '--log-level=warning'])
run('sentry-cli', ['sourcemaps', 'inject', 'dist'])
run('sentry-cli', ['sourcemaps', 'upload', '--release', 'shop@2.0.0', 'dist'], {
  ...process.env, SENTRY_URL: url, SENTRY_AUTH_TOKEN: token, SENTRY_ORG: 'davy', SENTRY_PROJECT: 'shop', SENTRY_DISABLE_UPDATE_CHECK: '1',
})
fs.symlinkSync(`${here}node_modules`, `${work}/node_modules`, 'dir')
const app = await import(`${work}/dist/app.min.js`)
await app.run(dsn)
