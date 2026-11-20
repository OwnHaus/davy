# Connect a Node app

Davy takes errors from the official Sentry SDK for Node. Only the DSN changes.

You need the DSN that `davy project create` printed in [install step 9](../install.md#9-connect-the-app). Put it into the app's settings as `SENTRY_DSN`, next to its other environment variables (`.env`, the deploy config or the process manager). The DSN is not a secret. It only lets a client send events.

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

Start the app with `node --import ./instrument.mjs app.mjs`. With CommonJS, use `instrument.js` with `require('@sentry/node')` and start with `node --require ./instrument.js app.js`.

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
