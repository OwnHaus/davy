# Connect a Node app

Davy takes errors from the official Sentry SDK for Node. Only the DSN changes.

You need the DSN that `davy project create` printed in [install step 9](../install.md#9-connect-the-app). Put it into the app's settings as `SENTRY_DSN`, next to its other environment variables (`.env`, the deploy config or the process manager), so the app has it every time it starts and not only in the shell that follows this page. An app with no such place yet gets a `.env` file in its directory with the line `SENTRY_DSN=<the DSN>`, which its start command loads (step 2). The DSN is not a secret. It only lets a client send events.

## 1. Install the SDK

In the app's directory:

```sh
npm install @sentry/node
```

Use the app's package manager if it is not npm (`pnpm add @sentry/node`, `yarn add @sentry/node`).

## 2. Start it before the app

Create `instrument.mjs` and load it before any other module:

```js
import * as Sentry from '@sentry/node'

Sentry.init({
  dsn: process.env.SENTRY_DSN,
  release: process.env.RELEASE,
  environment: process.env.NODE_ENV,
})
```

Start the app with `node --env-file-if-exists=.env --import ./instrument.mjs app.mjs`, and make that the `start` script in `package.json` when the app has one. `--env-file-if-exists` needs Node 22.9 or newer and does nothing when there is no `.env`. A variable already set in the environment wins over the file. With CommonJS, use `instrument.js` with `require('@sentry/node')` and start with `node --require ./instrument.js app.js`.

`release` is optional. With it, an issue marked fixed in one release reopens only when the error comes back in that release or a newer one.

## 3. Check that errors arrive

Send one test error and list the issues:

```sh
node --input-type=module -e "
import * as Sentry from '@sentry/node'
Sentry.init({ dsn: process.env.SENTRY_DSN })
Sentry.captureException(new Error('Davy test error'))
if (!(await Sentry.flush(5000))) throw new Error('the error was not sent, check SENTRY_DSN and that the server answers')
"
sudo runuser -u davy -- davy issues --data /var/lib/davy --project my-app
```

The last command prints a line with `Error: Davy test error`. If it prints nothing, run `sudo runuser -u davy -- davy doctor --data /var/lib/davy` and check that `SENTRY_DSN` holds the DSN of `my-app`.

## Logs

To send logs too, add `enableLogs: true` to `Sentry.init` and call `Sentry.logger.info(...)`. For pino, see [logtail.md](logtail.md).

## Cron jobs

A job wrapped in `Sentry.withMonitor` becomes a heartbeat monitor in Davy, with the same incidents and alerts as one made with `davy monitor add --heartbeat`:

```js
await Sentry.withMonitor('nightly-report', () => buildReport(), {
  schedule: { type: 'crontab', value: '0 3 * * *' },
  checkinMargin: 5,
  maxRuntime: 30,
  timezone: 'Europe/Prague',
})
```

The first check-in with a `schedule` creates the monitor `nightly-report` in the project of the DSN, and a later one with a different schedule updates it. A schedule is a crontab of 5 fields or `{ type: 'interval', value: 2, unit: 'hour' }`. Davy opens an incident when no check-in arrives by the scheduled time plus `checkinMargin` minutes (default 1), when a run does not end within `maxRuntime` minutes (default 30), or when the job throws. The next run that ends well resolves it. `Sentry.captureCheckIn` works the same way for a job that reports its own start and end.

Check-ins can create up to 100 monitors per project, and each monitor takes 6 results a minute. A check-in that Davy refuses, such as one for a slug with no monitor and no schedule, is counted in the `client_reports` table under the category `monitor`, with the reason, for example `davy_unknown_monitor`.

A monitor whose schedule you want to own, so that no check-in changes it, is made from the CLI with a slug. Its check-ins ping it, and their schedules are ignored:

```sh
sudo runuser -u davy -- davy monitor add my-app backup --heartbeat --every 1d --slug backup --data /var/lib/davy
```
