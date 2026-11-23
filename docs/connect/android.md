# Connect an Android app

Use the official Sentry Android SDK with Kotlin. Run `davy connect my-app
--data DATA --sdk android` on the Davy host. Use the public DSN for a phone,
with working TLS and a reachable hostname. `--local` points at loopback of
the device running the app, which is not the Davy host.

## 1. Install

In the app module's `build.gradle.kts` dependencies:

```kotlin
implementation("io.sentry:sentry-android:8.65.0")
```

The [official manual setup](https://docs.sentry.io/platforms/android/manual-setup/)
currently documents 8.65.0. Davy has no Android SDK version verified by its
e2e suite yet. The SDK version above is a documented setup version, not a
claim that it has passed an Android-to-Davy test. Report the version when
you verify your device. Only error envelopes and sessions are relevant;
Davy does not process native crash attachments or performance traces.

## 2. Keep the DSN outside committed configuration

Add `SENTRY_DSN=https://KEY@davy.example.com/1` to ignored `local.properties`
at the project root. In the app module's Gradle script:

```kotlin
val local = java.util.Properties().apply {
    rootProject.file("local.properties").inputStream().use { load(it) }
}
android {
    buildFeatures { buildConfig = true }
    defaultConfig {
        val dsn = local.getProperty("SENTRY_DSN", "")
        require(dsn.none { it == '"' || it == '\\' || it.isISOControl() })
        buildConfigField("String", "SENTRY_DSN", "\"$dsn\"")
    }
}
```

Disable automatic SDK init in the manifest's application element:

```xml
<meta-data android:name="io.sentry.auto-init" android:value="false" />
```

In your registered `Application` subclass, initialize once in `onCreate`:

```kotlin
import io.sentry.android.core.SentryAndroid

// Inside onCreate, after super.onCreate():
SentryAndroid.init(this) { options ->
    options.dsn = BuildConfig.SENTRY_DSN
}
```

The DSN will be in the APK, since it is a public ingest key. Keep the admin
MCP token out of the app entirely. CI should provide the same local build
property from its environment. Use valid HTTPS on a phone; plain HTTP may
also be blocked by Android's network security policy.

## 3. Check and handle offline devices

Capture once after initialization:

```kotlin
io.sentry.Sentry.captureException(IllegalStateException("Davy test error"))
```

Poll MCP `project_status` for the slug. Compare `event_count` with its
baseline, then use `issue_context` for the new issue. The returned curl
`test_event` command checks server ingest separately from the phone path.

The SDK [stores events on disk before sending](https://docs.sentry.io/platforms/android/configuration/draining/).
If your private Davy is unreachable until a VPN or Tailscale reconnects,
unsent events can be retried, including on the next app start. Caching is
bounded: [maxCacheItems](https://docs.sentry.io/platforms/android/configuration/options/#maxCacheItems)
defaults to 30 envelopes and evicts the oldest when full. It is not an
unlimited offline archive. Reconnect and reopen the app before checking.

## R8 and ProGuard

Davy only symbolicates JavaScript source maps today. It cannot accept or
apply R8/ProGuard mapping files to de-obfuscate Android stacks. Use debug
builds or set `isMinifyEnabled = false` (`minifyEnabled false` in Groovy)
for now. Do not configure the Sentry Gradle plugin to upload mapping files
to Davy.
