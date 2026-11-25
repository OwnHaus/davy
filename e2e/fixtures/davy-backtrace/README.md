# Backtraces of Davy's own panics

What `std::backtrace::Backtrace::force_capture().to_string()` gave in
Davy's panic hook for `davy crash`, in a release binary built as
`scripts/build-release` builds it and run from a directory other than the
one it was built in. `src/self_report/crash_tests.rs` reads them.

- `linux-aarch64-musl.txt`: built by `scripts/build-release
  aarch64-unknown-linux-musl` with Rust 1.99.1, run in `alpine:3.22` from
  `/home/op/elsewhere`. It has the line tables of the crate `davy`, with
  the build directory remapped to `.` and the cargo home to `/cargo`.
- `macos-aarch64.txt`: built by `scripts/build-release aarch64-apple-darwin`
  with Rust 1.99.1 on macOS 26, run from another directory. It has no line
  tables, so its frames name functions only.

To capture one again, make the hook write the text it parses to a file
next to the crash file for one build, run `davy crash --data DATA` from
another directory, and copy the file here.
