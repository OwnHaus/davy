# Fuzzing

One `cargo fuzz` target per parser that reads untrusted bytes, as `SECURITY.md` lists them.

| Target | What it feeds |
|---|---|
| `envelope` | `Envelope::parse`, then each item as an event and its grouping |
| `event` | `ErrorEvent::parse` on JSON, then its grouping |
| `sentry_log` | the Sentry `log` item |
| `logtail_json` | Logtail JSON and NDJSON bodies |
| `logtail_msgpack` | Logtail msgpack bodies |
| `otlp_protobuf`, `otlp_json` | OTLP log export requests |
| `artifact_bundle` | the zip `sentry-cli` uploads, with a 1 MiB unpack limit |
| `sourcemap` | a source map, a minified file and a frame position, through symbolication |

`fuzz/seed` fills `fuzz/corpus/` from the requests in `e2e/fixtures`.

## Running

`fuzz/docker SECONDS [TARGET...]` runs the targets at once in `rust:bookworm` with nightly, so it works on macOS. `FUZZ_CPUS` caps the cores it takes (default 6). A run before a release takes 3600 seconds per target. CI runs each target for five minutes every night (`.github/workflows/fuzz.yml`).

With nightly and `cargo-fuzz` installed, `cargo +nightly fuzz run TARGET` in this directory runs one target directly.

## A crash

libFuzzer writes the input to `fuzz/artifacts/TARGET/`. The fix comes with a unit test that feeds that input to the parser and fails without the fix, and the input goes into the test or into `e2e/fixtures` when it is a whole request.
