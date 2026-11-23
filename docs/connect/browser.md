# Connect browser JavaScript, with source maps

Davy takes errors from the official Sentry SDK for the browser and turns minified frames back into the original source with maps uploaded by `sentry-cli`.

## 1. Install and start the SDK

```sh
npm install @sentry/browser
```

In the app's entry module, before anything else:

```js
import * as Sentry from '@sentry/browser'

Sentry.init({ dsn: 'https://KEY@davy.example.com/1', release: 'my-app@1.0.0' })
```

Use the DSN from [install step 9](../install.md#9-connect-the-app). It is public by design, so it can sit in the bundle. The page must be allowed to reach the public URL: add it to `connect-src` if the site has a Content Security Policy.

## 2. Upload source maps when you build

`sentry-cli` uploads maps to Davy as it would to Sentry. Run `inject` on the built files before you deploy them, so each error carries the debug ID that ties it to its map:

<!-- e2e: skip, e2e/run.sh runs sentry-cli against the binary -->
```sh
export SENTRY_URL=https://davy.example.com SENTRY_ORG=any SENTRY_PROJECT=my-app
npx @sentry/cli sourcemaps inject dist
npx @sentry/cli sourcemaps upload --release my-app@1.0.0 dist
```

`sentry-cli` reads the admin token from `SENTRY_AUTH_TOKEN`. Store the contents of `/var/lib/davy/token` as a secret of the CI job under that name. Never write it into the build script or the repository. Davy matches maps by debug ID only, so the org, project and release names do not matter.

Do not deploy the `.map` files to the public site. `inject` and `upload` need them only during the build.

## 3. Check

Throw an error in the browser console of the deployed site, then:

<!-- e2e: skip, needs a browser -->
```sh
sudo runuser -u davy -- davy issues --data /var/lib/davy --project my-app
```

`davy context ID --data /var/lib/davy` shows the stack with the original file names and lines.

## Optional: user feedback

The SDK's feedback widget sends what users write to Davy without other changes. Add it to the integrations:

```js
Sentry.init({
  dsn: 'https://KEY@davy.example.com/1',
  release: 'my-app@1.0.0',
  integrations: [Sentry.feedbackIntegration({ colorScheme: 'system' })],
})
```

To ask after an error instead, call `Sentry.captureFeedback({ message, name, email, associatedEventId })` with the ID `Sentry.captureException` returned. Feedback about an error shows up in `davy context ID` under `feedback`. All of it is in the `user_feedback` table, which `davy sql` reads. Davy keeps up to 1,000 messages per project and day.
