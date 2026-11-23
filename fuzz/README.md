# Fuzzing

One `cargo fuzz` target per parser that reads untrusted bytes, as `SECURITY.md` lists them.

| Target | What it feeds |
|---|---|
| `envelope` | `Envelope::parse`, then each item as an event and its grouping |
| `event` | `ErrorEvent::parse` on JSON, then its grouping |
| `sentry_log` | the Sentry `log` item |
| `check_in` | the Sentry `check_in` item and its schedule, stored and read back, and bytes as a crontab |
| `logtail_json` | Logtail JSON and NDJSON bodies |
| `logtail_msgpack` | Logtail msgpack bodies |
| `otlp_protobuf`, `otlp_json` | OTLP log export requests |
| `artifact_bundle` | the zip `sentry-cli` uploads, with a 1 MiB unpack limit |
| `sourcemap` | a source map, a minified file and a frame position, through symbolication |

`fuzz/seed` fills `fuzz/corpus/` from the requests in `e2e/fixtures`.

## Running

`fuzz/run SECONDS [TARGET...]` runs the targets at once, all of them when none is named. It needs nightly and `cargo-fuzz`. `fuzz/docker` takes the same arguments and runs it in `rust:bookworm`, so it works on macOS without either. `FUZZ_CPUS` caps the cores the container takes (default 6). A run before a release takes 3600 seconds per target. CI runs `fuzz/run 300` every night (`.github/workflows/fuzz.yml`).

## A crash

libFuzzer writes the input to `fuzz/artifacts/TARGET/`. The fix comes with a unit test that feeds that input to the parser and fails without the fix, and the input goes into the test or into `e2e/fixtures` when it is a whole request.
