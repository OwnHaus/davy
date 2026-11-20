// Records what sentry-cli sends when it uploads source maps, and the error event
// that @sentry/node sends from the uploaded bundle. Writes e2e/fixtures/sentry-cli-3/.
import http from 'node:http'
import fs from 'node:fs'
import zlib from 'node:zlib'
import os from 'node:os'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const here = fileURLToPath(new URL('.', import.meta.url))
const out = `${here}fixtures/sentry-cli-3/`
const work = process.argv[2]
fs.rmSync(out, { recursive: true, force: true })
fs.mkdirSync(out, { recursive: true })
fs.mkdirSync(`${work}/src`, { recursive: true })
fs.copyFileSync(`${here}shop/app.js`, `${work}/src/app.js`)

let n = 0
let assembled = false
function record(req, body) {
  n += 1
  const name = String(n).padStart(2, '0')
  fs.writeFileSync(`${out}${name}.meta.json`, JSON.stringify({ method: req.method, url: req.url, headers: { ...req.headers, host: 'dev-host', authorization: req.headers.authorization && 'Bearer <token>' } }, null, 2) + '\n')
  fs.writeFileSync(`${out}${name}.body`, body)
}

const server = http.createServer((req, res) => {
  const chunks = []
  req.on('data', c => chunks.push(c))
  req.on('end', () => {
    let body = Buffer.concat(chunks)
    if (req.headers['content-encoding'] === 'gzip') body = zlib.gunzipSync(body)
    if (req.url.includes('/envelope/')) body = Buffer.from(body.toString('utf8').replaceAll(os.homedir(), '/home/dev').replaceAll(os.hostname(), 'dev-host'))
    record(req, body)
    const reply = (status, value) => {
      res.writeHead(status, { 'content-type': 'application/json' })
      res.end(JSON.stringify(value))
    }
    const base = `http://127.0.0.1:${server.address().port}`
    if (req.method === 'GET' && req.url.endsWith('/chunk-upload/')) {
      return reply(200, {
        url: `${base}${req.url}`, chunkSize: 8388608, chunksPerRequest: 64, maxFileSize: 2147483648,
        maxRequestSize: 33554432, concurrency: 1, hashAlgorithm: 'sha1', compression: ['gzip'],
        accept: ['artifact_bundles', 'debug_files', 'release_files', 'sources', 'bcsymbolmaps', 'il2cpp', 'portablepdbs', 'proguard'],
      })
    }
    if (req.method === 'POST' && req.url.endsWith('/chunk-upload/')) return reply(200, {})
    if (req.method === 'POST' && req.url.endsWith('/artifactbundle/assemble/')) {
      const { chunks } = JSON.parse(body)
      if (assembled) return reply(200, { state: 'created', missingChunks: [] })
      assembled = true
      return reply(200, { state: 'not_found', missingChunks: chunks })
    }
    if (req.url.includes('/envelope/')) return reply(200, { id: '0' })
    return reply(404, { detail: 'not captured' })
  })
})

server.listen(0, async () => {
  const port = server.address().port
  const bin = `${here}node_modules/.bin/`
  execFileSync(`${bin}esbuild`, ['src/app.js', '--bundle', '--minify', '--sourcemap', '--platform=node', '--format=esm', '--external:sentry-node-10', '--outfile=dist/app.min.js'], { cwd: work, stdio: 'inherit' })
  execFileSync(`${bin}sentry-cli`, ['sourcemaps', 'inject', 'dist'], { cwd: work, stdio: 'inherit' })
  const env = { ...process.env, SENTRY_URL: `http://127.0.0.1:${port}/`, SENTRY_AUTH_TOKEN: 'sntrys_test', SENTRY_ORG: 'davy', SENTRY_PROJECT: 'shop' }
  const upload = (args) => new Promise((resolve, reject) => {
    import('node:child_process').then(({ execFile }) => execFile(`${bin}sentry-cli`, args, { cwd: work, env }, (error, stdout, stderr) => {
      process.stdout.write(stdout)
      process.stderr.write(stderr)
      error ? reject(error) : resolve()
    }))
  })
  try {
    await upload(['sourcemaps', 'upload', '--release', 'shop@2.0.0', 'dist'])
  } catch (error) {
    console.error('upload failed', error.message)
  }
  fs.symlinkSync(`${here}node_modules`, `${work}/node_modules`, 'dir')
  const { run } = await import(`${work}/dist/app.min.js`)
  await run(`http://abc123@127.0.0.1:${port}/1`)
  fs.copyFileSync(`${work}/dist/app.min.js`, `${out}app.min.js`)
  fs.copyFileSync(`${work}/dist/app.min.js.map`, `${out}app.min.js.map`)
  server.close()
  console.log('sentry-cli-3: captured', n, 'requests')
})
