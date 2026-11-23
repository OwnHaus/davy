use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, clap::ValueEnum, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(crate = "rmcp::schemars")]
pub enum Sdk {
    Android,
    Rust,
    Go,
    Node,
    Python,
    Dotnet,
    Browser,
}

#[derive(Debug, Serialize)]
pub struct Instructions {
    pub package: &'static str,
    pub install: &'static str,
    pub init: &'static str,
    pub dsn_location: &'static str,
}

impl Sdk {
    pub fn instructions(self) -> Instructions {
        let (package, install, init, location) = match self {
            Self::Android => (
                "io.sentry:sentry-android",
                "implementation(\"io.sentry:sentry-android:8.65.0\")",
                "SentryAndroid.init(this) { options -> options.dsn = BuildConfig.SENTRY_DSN }",
                "local.properties (gitignored); load into BuildConfig.SENTRY_DSN at build time",
            ),
            Self::Rust => (
                "sentry",
                "cargo add sentry@0.49",
                "let _guard = sentry::init(sentry::ClientOptions::new().dsn(std::env::var(\"SENTRY_DSN\").ok().and_then(|s| s.parse().ok())));",
                ".env (gitignored), exported by the launcher; or systemd drop-in Environment=SENTRY_DSN=...",
            ),
            Self::Go => (
                "github.com/getsentry/sentry-go",
                "go get github.com/getsentry/sentry-go",
                "if err := sentry.Init(sentry.ClientOptions{Dsn: os.Getenv(\"SENTRY_DSN\")}); err != nil { log.Fatal(err) }; defer sentry.Flush(2 * time.Second)",
                ".env (gitignored), exported by the launcher; or systemd drop-in Environment=SENTRY_DSN=...",
            ),
            Self::Node => (
                "@sentry/node",
                "npm install @sentry/node",
                "import * as Sentry from '@sentry/node';\nSentry.init({ dsn: process.env.SENTRY_DSN });",
                ".env (gitignored); load with node --env-file=.env or the deployment environment",
            ),
            Self::Python => (
                "sentry-sdk",
                "pip install sentry-sdk",
                "import os, sentry_sdk\nsentry_sdk.init(dsn=os.environ['SENTRY_DSN'])",
                ".env (gitignored), exported by the launcher; or systemd drop-in Environment=SENTRY_DSN=...",
            ),
            Self::Dotnet => (
                "Sentry.AspNetCore",
                "dotnet add package Sentry.AspNetCore",
                "builder.WebHost.UseSentry(options => { options.Dsn = Environment.GetEnvironmentVariable(\"SENTRY_DSN\") ?? \"\"; });",
                ".env (gitignored), exported by the launcher; or systemd drop-in Environment=SENTRY_DSN=...",
            ),
            Self::Browser => (
                "@sentry/browser",
                "npm install @sentry/browser",
                "import * as Sentry from '@sentry/browser';\nSentry.init({ dsn: import.meta.env.VITE_SENTRY_DSN });",
                ".env.local (gitignored), VITE_SENTRY_DSN for Vite; use the equivalent public build variable for another bundler",
            ),
        };
        Instructions {
            package,
            install,
            init,
            dsn_location: location,
        }
    }
}
