"""Sums up one eval run from the files run.sh left in its directory."""
import json
import sys
from pathlib import Path

out = Path(sys.argv[1])


def read_json(name):
    try:
        return json.loads((out / name).read_text())
    except (OSError, ValueError):
        return None


doctor = read_json('doctor.json')
mcp = read_json('mcp.json')
usage = {}
for line in (out / 'events.jsonl').read_text().splitlines():
    try:
        event = json.loads(line)
    except ValueError:
        continue
    if event.get('type') in ('turn.completed', 'result'):
        for key, value in event.get('usage', {}).items():
            if isinstance(value, int):
                usage[key] = usage.get(key, 0) + value
    if event.get('type') == 'result':
        (out / 'last.txt').write_text(str(event.get('result', '')))
verdict = {
    'run': out.name,
    'seconds': int((out / 'seconds').read_text()),
    'exit': int((out / 'exit').read_text()),
    'doctor_ok': bool(doctor) and all(c['status'] != 'fail' for c in doctor['checks']),
    'doctor_not_ok': {c['name']: c['status'] for c in (doctor or {}).get('checks', []) if c['status'] != 'ok'},
    'test_error_over_mcp': bool(mcp and mcp['test_error']),
    'app_error_over_mcp': bool(mcp and mcp['app_error']),
    'agent_mcp_configured': 'davy' in (out / 'agent-mcp.txt').read_text(),
    'feedback': read_json('feedback.json'),
    'usage': usage,
}
verdict['pass'] = verdict['doctor_ok'] and verdict['test_error_over_mcp'] and verdict['app_error_over_mcp']
(out / 'verdict.json').write_text(json.dumps(verdict, indent=2) + '\n')
print(json.dumps(verdict))
