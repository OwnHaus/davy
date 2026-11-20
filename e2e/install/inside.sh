#!/bin/bash
set -euo pipefail
# docker exec starts with umask 0; a login shell has 022.
umask 022
install -m 755 /target/davy-linux /usr/local/bin/davy
useradd --create-home --shell /bin/bash agent
echo 'agent ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/agent
chmod 440 /etc/sudoers.d/agent
install -d -o agent -g agent /app
cp /e2e/mcp-check.mjs /app/
runuser -u agent -- bash -c 'cd /app && npm install --silent --no-audit --no-fund @modelcontextprotocol/sdk@1 > /dev/null'
python3 /e2e/follow.py
