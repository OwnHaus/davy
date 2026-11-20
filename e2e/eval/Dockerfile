FROM node:22-bookworm
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends systemd systemd-sysv sudo git ca-certificates && rm -rf /var/lib/apt/lists/*
ARG CODEX_VERSION
RUN npm install -g --silent "@openai/codex@${CODEX_VERSION}"
RUN mkdir /opt/check && cd /opt/check && npm init -y > /dev/null && npm install --silent --no-audit --no-fund @modelcontextprotocol/sdk@1
STOPSIGNAL SIGRTMIN+3
CMD ["/lib/systemd/systemd"]
