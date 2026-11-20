"""Runs the sh blocks of the install docs in order, as a user who may use
sudo, the way a coding agent follows them. A block directly after a line
that starts with `<!-- e2e: skip` is left out. Then checks the result."""

import json
import os
import subprocess
import sys

PAGES = [('/docs/install.md', '/home/agent'), ('/docs/connect/node.md', '/app'), ('/docs/connect/agents.md', '/app')]
PUBLIC_URL = 'https://davy.example.com'
TEST_URL = 'http://127.0.0.1:8000'


def blocks(path):
    lines = open(path).read().split('\n')
    i = 0
    while i < len(lines):
        if lines[i] == '```sh':
            before = next((l for l in reversed(lines[:i]) if l.strip()), '')
            end = lines.index('```', i + 1)
            if not before.startswith('<!-- e2e: skip'):
                yield i + 1, '\n'.join(lines[i + 1:end])
            i = end
        i += 1


def run(cmd, cwd, env, user='agent'):
    return subprocess.run(['runuser', '-u', user, '--', *cmd], cwd=cwd, env=env, capture_output=True, text=True)


env = {'PATH': os.environ['PATH'], 'LANG': 'C.UTF-8'}
outputs = {}
for page, cwd in PAGES:
    for line, code in blocks(page):
        code = code.replace(PUBLIC_URL, TEST_URL)
        print(f'--- {page}:{line}', flush=True)
        print(code, flush=True)
        r = run(['bash', '-e', '-o', 'pipefail', '-c', code], cwd, env)
        print(r.stdout + r.stderr, flush=True)
        if r.returncode != 0:
            sys.exit(f'FAIL {page}:{line} exited {r.returncode}')
        if 'davy project create' in code:
            env['SENTRY_DSN'] = r.stdout.strip().split('\n')[-1]
        if 'davy mcp-config' in code:
            outputs['mcp-config'] = r.stdout
        if 'davy issues' in code:
            outputs['issues'] = r.stdout

doctor = run(['davy', 'doctor', '--data', '/var/lib/davy', '--json'], '/', env, user='davy')
checks = json.loads(doctor.stdout)['checks']
bad = [c for c in checks if c['status'] != 'ok']
assert not bad, bad
print('verified: doctor reports', ', '.join(c['name'] for c in checks), 'as ok')

assert 'Error: Davy test error' in outputs['issues'], outputs['issues']
print('verified: the test error from node.md is an issue')

token = open('/var/lib/davy/token').read().strip()
shell = run(['bash', '-ic', 'printf %s "$DAVY_TOKEN"'], '/app', env)
assert shell.stdout == token, 'DAVY_TOKEN in a new shell differs from /var/lib/davy/token'
print('verified: a new shell has DAVY_TOKEN from agents.md')

config = outputs['mcp-config']
with open('/app/.mcp.json', 'w') as f:
    f.write(config[config.index('{'):])
check = run(['node', 'mcp-check.mjs', '.mcp.json'], '/app', {**env, 'DAVY_TOKEN': token})
print(check.stdout + check.stderr, flush=True)
assert check.returncode == 0, 'the MCP check failed'
