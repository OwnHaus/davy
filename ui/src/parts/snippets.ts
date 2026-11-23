// What each SDK needs to send to Davy, filled in with this server's URL and
// the picked project's DSN. The words follow docs/connect, which e2e runs.

export const SDKS = ["node", "browser", "python", "go", "logtail", "otel", "agents"] as const;
export type Sdk = (typeof SDKS)[number];

export const SDK_NAMES: Record<Sdk, string> = {
  node: "Node",
  browser: "Browser",
  python: "Python",
  go: "Go",
  logtail: "Logtail",
  otel: "OpenTelemetry",
  agents: "Agents",
};

export type Fill = {
  /** The public URL, without a trailing slash. */
  url: string;
  logs: string;
  otlp: string;
  slug: string;
  dsn: string;
  key: string;
};

export type Block = { label: string; code: string };
export type Guide = { intro: string; blocks: Block[]; doc: string };

export function guide(sdk: Sdk, f: Fill): Guide {
  switch (sdk) {
    case "node":
      return {
        intro: "The Sentry SDK for Node sends errors to Davy. Load it before the app, so it sees every module.",
        blocks: [
          { label: "Install", code: "npm install @sentry/node" },
          { label: ".env", code: `SENTRY_DSN=${f.dsn}` },
          {
            label: "instrument.mjs",
            code: [
              "import * as Sentry from '@sentry/node'",
              "",
              "Sentry.init({",
              "  dsn: process.env.SENTRY_DSN,",
              "  release: process.env.RELEASE,",
              "  environment: process.env.NODE_ENV,",
              "})",
            ].join("\n"),
          },
          { label: "Start", code: "node --env-file-if-exists=.env --import ./instrument.mjs app.mjs" },
        ],
        doc: "docs/connect/node.md",
      };
    case "browser":
      return {
        intro:
          "The Sentry SDK for the browser sends errors to Davy. Upload the source maps of each release, so a stack shows your source. The upload takes the admin token as SENTRY_AUTH_TOKEN, kept as a CI secret.",
        blocks: [
          { label: "Install", code: "npm install @sentry/browser" },
          {
            label: "main.js",
            code: [
              "import * as Sentry from '@sentry/browser'",
              "",
              `Sentry.init({ dsn: '${f.dsn}', release: '${f.slug}@1.0.0' })`,
            ].join("\n"),
          },
          {
            label: "Source maps, after each build",
            code: [
              `export SENTRY_URL=${f.url} SENTRY_ORG=any SENTRY_PROJECT=${f.slug}`,
              "npx @sentry/cli sourcemaps inject dist",
              `npx @sentry/cli sourcemaps upload --release ${f.slug}@1.0.0 dist`,
            ].join("\n"),
          },
        ],
        doc: "docs/connect/browser.md",
      };
    case "python":
      return {
        intro: "The Sentry SDK for Python sends errors to Davy. Initialize it before the app starts.",
        blocks: [
          { label: "Install", code: "pip install sentry-sdk" },
          { label: "Environment", code: `export SENTRY_DSN=${f.dsn}` },
          {
            label: "app.py",
            code: [
              "import os",
              "import sentry_sdk",
              "",
              "sentry_sdk.init(",
              '    dsn=os.environ["SENTRY_DSN"],',
              '    release=os.environ.get("RELEASE"),',
              '    environment=os.environ.get("ENVIRONMENT"),',
              ")",
            ].join("\n"),
          },
        ],
        doc: "docs/connect/python.md",
      };
    case "go":
      return {
        intro: "The Sentry SDK for Go sends errors to Davy. Flush it before the program exits, or the last errors stay unsent.",
        blocks: [
          { label: "Install", code: "go get github.com/getsentry/sentry-go" },
          { label: "Environment", code: `export SENTRY_DSN=${f.dsn}` },
          {
            label: "main.go",
            code: [
              "err := sentry.Init(sentry.ClientOptions{",
              '\tDsn:     os.Getenv("SENTRY_DSN"),',
              '\tRelease: os.Getenv("RELEASE"),',
              "})",
              "if err != nil {",
              '\tlog.Fatalf("sentry: %v", err)',
              "}",
              "defer sentry.Flush(2 * time.Second)",
            ].join("\n"),
          },
        ],
        doc: "docs/connect/go.md",
      };
    case "logtail":
      return {
        intro:
          "Davy takes logs in the Logtail format. A shipper that sends to Better Stack sends here with two settings changed: the endpoint and the source token, which is the project key. The key only lets a client send logs to this project.",
        blocks: [
          { label: "Environment", code: `export DAVY_LOG_KEY=${f.key}` },
          { label: "Install", code: "npm install pino @logtail/pino" },
          {
            label: "log.js",
            code: [
              "import pino from 'pino'",
              "",
              "const transport = pino.transport({",
              "  target: '@logtail/pino',",
              "  options: {",
              "    sourceToken: process.env.DAVY_LOG_KEY,",
              `    options: { endpoint: '${f.url}' },`,
              "  },",
              "})",
              "export const log = pino(transport)",
            ].join("\n"),
          },
          {
            label: "Or a plain POST",
            code: [
              `curl -fsS -X POST ${f.logs} -H "Authorization: Bearer $DAVY_LOG_KEY" \\`,
              `  -H 'Content-Type: application/json' -d '{"message":"payment timed out","level":"error"}'`,
            ].join("\n"),
          },
        ],
        doc: "docs/connect/logtail.md",
      };
    case "otel":
      return {
        intro: "An OpenTelemetry SDK or Collector sends logs to Davy over OTLP with HTTP. The project key goes in the Authorization header.",
        blocks: [
          {
            label: "Environment",
            code: [
              `export DAVY_LOG_KEY=${f.key}`,
              `export OTEL_EXPORTER_OTLP_LOGS_ENDPOINT=${f.otlp}`,
              'export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer $DAVY_LOG_KEY"',
            ].join("\n"),
          },
        ],
        doc: "docs/connect/opentelemetry.md",
      };
    case "agents":
      return {
        intro:
          "A coding agent reads issues and logs through Davy's MCP server with the admin token. The token goes into a file only you can read and reaches the agent through DAVY_TOKEN. Use ~/.zshrc on a Mac. Use --client codex or --client cursor for those agents, then start the agent again.",
        blocks: [
          {
            label: "On the Davy server",
            code: [
              "mkdir -p ~/.config/davy",
              "(umask 077 && sudo runuser -u davy -- cat /var/lib/davy/token > ~/.config/davy/token)",
              `echo 'export DAVY_TOKEN="$(cat ~/.config/davy/token)"' >> ~/.bashrc`,
            ].join("\n"),
          },
          { label: "In the project the agent works on", code: `davy mcp-config --client claude-code --url ${f.url}` },
        ],
        doc: "docs/connect/agents.md",
      };
  }
}
