//! Davy's own panics, as issues of Davy's own project. The panic hook
//! writes a crash file into DATA/crashes before anything else happens, so
//! a panic that ends the process, or one in the log writer or the store
//! writer, is not lost. The forwarder records the files, at the next start
//! or within seconds while the process lives, and deletes each one once
//! the store has it, see [`ingest`]. "Self-monitoring" in docs/design.md
//! has the rules.

use super::{MAX_MESSAGE, capped, quiet, stderr};
use crate::event::EventId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    backtrace::Backtrace,
    fs,
    io::{self, Write as _},
    panic::PanicHookInfo,
    path::{Path, PathBuf},
    sync::OnceLock,
};

mod ingest;
pub use ingest::{
    Listing, MAX_ENTRIES, REFUSED, STALE_TMP, Target, Told, list, pending, read, record, sweep,
};

/// The directory of the data directory that holds crash files.
pub const DIR: &str = "crashes";
/// Crash files that wait at most. A panic past them is printed only, so a
/// panic that repeats cannot fill the disk.
pub const MAX_PENDING: usize = 20;
/// Frames kept of a backtrace, the innermost ones.
pub const MAX_FRAMES: usize = 100;
/// The largest crash file written or read, in bytes.
pub const MAX_FILE: usize = 64 * 1024;
/// The version of the crash file format.
pub const FORMAT: u32 = 1;
/// Crash files recorded at once, so a backlog cannot hold up the lines.
pub const PER_SCAN: usize = 10;
/// The longest function name, file or thread name kept, in bytes.
const MAX_NAME: usize = 512;
/// The crate whose frames are in-app.
const CRATE: &str = "davy";
const COMPONENT: &str = "crash";
/// This file, as its frames name it relative to the directory Davy was
/// built in. A frame of the panic hook ends with it, and what comes before
/// is that directory, which [`frames`] strips from every file.
const ANCHOR: &str = file!();

/// One panic as the hook saw it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub format: u32,
    pub event_id: String,
    pub at_ms: i64,
    /// `davy@` and the version that panicked, which may be older than the
    /// one that records it.
    pub release: String,
    pub thread: String,
    pub message: String,
    /// `file:line:column` of the panic.
    pub location: Option<String>,
    /// The oldest call first, as Sentry orders frames.
    pub frames: Vec<Frame>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub function: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

impl Report {
    /// The report of a panic with `message` at `location` on `thread`, and
    /// the frames of `backtrace`, the text of a [`Backtrace`].
    pub fn new(
        message: &str,
        location: Option<String>,
        thread: &str,
        backtrace: &str,
        at_ms: i64,
        event_id: String,
    ) -> Self {
        Self {
            format: FORMAT,
            event_id,
            at_ms,
            release: format!("{CRATE}@{}", env!("CARGO_PKG_VERSION")),
            thread: capped(thread, MAX_NAME).to_owned(),
            message: capped(message, MAX_MESSAGE).to_owned(),
            location: location.map(|l| capped(&l, MAX_NAME).to_owned()),
            frames: frames(backtrace),
        }
    }
}

/// The crash files of `data`.
pub fn dir(data: &Path) -> PathBuf {
    data.join(DIR)
}

static HOOK: OnceLock<PathBuf> = OnceLock::new();

/// Saves every later panic of this process in DATA/crashes, then runs the
/// hook set before, which prints the panic. The first call sets the hook,
/// and a later one changes nothing and returns false.
pub fn install_hook(data: &Path) -> bool {
    if HOOK.set(dir(data)).is_err() {
        return false;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        save(info);
        previous(info);
    }));
    true
}

/// Saves a panic, the test `davy crash` asks for, in DATA/crashes, as a
/// panic of `davy serve` would be.
///
/// # Panics
///
/// Always, that is what it is for.
#[expect(clippy::panic, reason = "a panic is what `davy crash` asks for")]
pub fn test_panic(data: &Path) -> ! {
    install_hook(data);
    stderr(
        COMPONENT,
        "this panic is a test, asked for with `davy crash`; `davy serve` records it as an issue of Davy's own project",
    );
    panic!("a test panic, asked for with `davy crash`");
}

/// Writes the crash file of a panic. A failure is printed: a panic hook
/// must not panic, and has nowhere else to say it.
fn save(info: &PanicHookInfo<'_>) {
    let Some(dir) = HOOK.get() else {
        return;
    };
    let _quiet = quiet();
    let message = info.payload_as_str().unwrap_or("a panic without a message");
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
    let current = std::thread::current();
    let report = Report::new(
        message,
        location,
        current.name().unwrap_or("unnamed"),
        &Backtrace::force_capture().to_string(),
        crate::time::now_ms(),
        EventId::fresh().0,
    );
    match write(dir, &report) {
        Ok(path) => stderr(
            COMPONENT,
            format_args!("the panic is saved in {}", path.display()),
        ),
        Err(e) => stderr(
            COMPONENT,
            format_args!("the panic was not saved in {}: {e}", dir.display()),
        ),
    }
}

/// The frames of the text of a [`Backtrace`], the oldest first, without
/// the frames of the panic machinery above the code that panicked and of
/// the runtime below the thread's own code. The innermost
/// [`MAX_FRAMES`] are kept. Davy's own files are named from the crate
/// root, such as `src/main.rs`, wherever Davy was built or runs, see
/// [`relative`].
pub fn frames(text: &str) -> Vec<Frame> {
    let mut frames: Vec<Frame> = Vec::new();
    for line in text.lines().map(str::trim_start) {
        if let Some(at) = line.strip_prefix("at ") {
            if let Some(frame) = frames.last_mut().filter(|f| f.file.is_none()) {
                (frame.file, frame.line, frame.column) = location(at);
            }
            continue;
        }
        let Some((n, name)) = line.split_once(": ") else {
            continue;
        };
        if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        // An address without a symbol: `0x1234 - <unknown>`.
        let name = name
            .strip_prefix("0x")
            .and_then(|rest| rest.split_once(" - "))
            .map_or(name, |(_, symbol)| symbol);
        frames.push(Frame {
            function: capped(name, MAX_NAME).to_owned(),
            file: None,
            line: None,
            column: None,
        });
    }
    relative(&mut frames);
    // The panic runs through `__rust_end_short_backtrace`, then the
    // functions that format and start it, then the code that panicked.
    let start = frames
        .iter()
        .position(|f| f.function.contains("__rust_end_short_backtrace"))
        .map_or(0, |i| i + 1);
    let start = frames[start..]
        .iter()
        .position(|f| !machinery(&f.function))
        .map_or(frames.len(), |i| start + i);
    let end = frames[start..]
        .iter()
        .position(|f| f.function.contains("__rust_begin_short_backtrace"))
        .map_or(frames.len(), |i| start + i);
    let mut kept: Vec<Frame> = frames.drain(start..end).take(MAX_FRAMES).collect();
    kept.reverse();
    kept
}

/// Names Davy's own files from the crate root. A backtrace names a file
/// as the build wrote it: relative to the build directory with `./` when
/// a release build remapped that directory, else with the build
/// directory's absolute path, or with `./` when Davy runs from it. The
/// frame of the panic hook in this file shows which: what comes before
/// [`ANCHOR`] is stripped from every file that starts with it, then a
/// leading `./`. The build directory is found at run time rather than
/// written into the binary, which would hold the build machine's path.
fn relative(frames: &mut [Frame]) {
    let root = frames
        .iter()
        .filter_map(|f| f.file.as_deref()?.strip_suffix(ANCHOR))
        .find(|root| root.len() > 1)
        .map(str::to_owned);
    for file in frames.iter_mut().filter_map(|f| f.file.as_mut()) {
        let stripped = root.as_deref().and_then(|root| file.strip_prefix(root));
        let stripped = stripped.unwrap_or(file);
        let stripped = stripped
            .strip_prefix("./")
            .or_else(|| stripped.strip_prefix(".\\"))
            .unwrap_or(stripped);
        if stripped.len() != file.len() {
            *file = stripped.to_owned();
        }
    }
}

/// Whether a frame belongs to the code that starts a panic.
fn machinery(function: &str) -> bool {
    [
        "std::panicking::",
        "core::panicking::",
        "__rustc::rust_begin_unwind",
        "rust_begin_unwind",
    ]
    .iter()
    .any(|prefix| function.starts_with(prefix))
}

/// The file, line and column of `path:line:column`.
fn location(at: &str) -> (Option<String>, Option<u32>, Option<u32>) {
    let file = |s: &str| Some(capped(s, MAX_NAME).to_owned());
    let number = |s: &str| s.parse::<u32>().ok();
    if let Some((rest, last)) = at.rsplit_once(':')
        && let Some(last) = number(last)
    {
        if let Some((path, line)) = rest.rsplit_once(':')
            && let Some(line) = number(line)
        {
            return (file(path), Some(line), Some(last));
        }
        return (file(rest), Some(last), None);
    }
    (file(at), None, None)
}

/// Writes `report` into `dir` as `{at_ms}-{event_id}.json`, readable by
/// its owner only, through a hidden file renamed into place, so a reader
/// never sees half a file. The innermost frames that fit in [`MAX_FILE`]
/// are kept.
pub fn write(dir: &Path, report: &Report) -> io::Result<PathBuf> {
    make_dir(dir)?;
    if pending(dir)?.len() >= MAX_PENDING {
        return Err(io::Error::other(format!(
            "{MAX_PENDING} crashes wait to be recorded already"
        )));
    }
    let bytes = encode(report)?;
    let name = format!("{:013}-{}.json", report.at_ms, report.event_id);
    let hidden = dir.join(format!(".{name}.tmp"));
    let path = dir.join(name);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&hidden).and_then(|mut file| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&hidden, &path)
    });
    if written.is_err() {
        fs::remove_file(&hidden).unwrap_or_default();
    }
    written?;
    // The file is in place. A directory that does not sync loses it only
    // if the machine stops before the next sync, so this is not a failure.
    #[cfg(unix)]
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .unwrap_or_default();
    Ok(path)
}

/// Makes `dir` for crash files, readable by this process's user only. One
/// that exists must be a directory of that user, not a link to one, and
/// is made readable by that user only.
fn make_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let me = nix::unistd::geteuid().as_raw();
        if meta.uid() != me {
            return Err(io::Error::other(format!(
                "{} belongs to user {}, not to this process's user {me}",
                dir.display(),
                meta.uid()
            )));
        }
        if meta.mode() & 0o777 != 0o700 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// `report` as JSON of at most [`MAX_FILE`] bytes, without its oldest
/// frames if they do not fit.
fn encode(report: &Report) -> io::Result<Vec<u8>> {
    let mut report = report.clone();
    loop {
        let bytes = serde_json::to_vec(&report)?;
        if bytes.len() <= MAX_FILE || report.frames.is_empty() {
            return Ok(bytes);
        }
        // The average frame is larger than a frame's share of the file, so
        // this drops no more than it must.
        let average = bytes.len().div_ceil(report.frames.len());
        let over = (bytes.len() - MAX_FILE).div_ceil(average);
        report.frames.drain(..over.min(report.frames.len()));
    }
}

/// The Sentry event of `report`: a fatal, unhandled exception of type
/// `panic` whose frames of the crate `davy` are in-app, so it groups like
/// any other error by its innermost in-app frames.
pub fn event(report: &Report) -> Value {
    let frames: Vec<Value> = report.frames.iter().map(frame).collect();
    let mut tags = Map::new();
    tags.insert("thread".to_owned(), report.thread.clone().into());
    if let Some(location) = &report.location {
        tags.insert("location".to_owned(), location.clone().into());
    }
    let version = report
        .release
        .strip_prefix("davy@")
        .unwrap_or(&report.release);
    json!({
        "event_id": report.event_id,
        "timestamp": crate::time::rfc3339_ms(report.at_ms),
        "platform": "rust",
        "level": "fatal",
        "logger": "panic",
        "release": report.release,
        "exception": {"values": [{
            "type": "panic",
            "value": report.message,
            "mechanism": {"type": "panic", "handled": false},
            "stacktrace": {"frames": frames},
        }]},
        "tags": tags,
        "contexts": {"runtime": {"name": CRATE, "version": version}},
    })
}

/// A frame of the crate `davy` is in-app. Its crate is the first part of
/// its function's path, or for a function inlined without a path, `davy`
/// when its file is under `src/`.
fn frame(f: &Frame) -> Value {
    let module =
        module(&f.function).or_else(|| f.file.as_deref().is_some_and(own_file).then_some(CRATE));
    let mut v = Map::new();
    v.insert("function".to_owned(), f.function.clone().into());
    if let Some(module) = module {
        v.insert("module".to_owned(), module.into());
    }
    if let Some(file) = &f.file {
        v.insert("filename".to_owned(), file.clone().into());
    }
    if let Some(line) = f.line {
        v.insert("lineno".to_owned(), line.into());
    }
    if let Some(column) = f.column {
        v.insert("colno".to_owned(), column.into());
    }
    v.insert("in_app".to_owned(), (module == Some(CRATE)).into());
    Value::Object(v)
}

/// The crate of a function name such as `davy::logs::State::add` or
/// `<davy::store::Writer>::start`. None for a name without a path, such
/// as `block_on<davy::main::{async_block_env#0}>`, which a frame inlined
/// in a release build has.
fn module(function: &str) -> Option<&str> {
    let name = function.trim_start_matches(['<', '&']);
    let name = name.strip_prefix("dyn ").unwrap_or(name);
    let (krate, _) = name.split_once("::")?;
    let ident = !krate.is_empty() && krate.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ident.then_some(krate)
}

/// Whether `file`, named from the crate root by [`frames`], is Davy's own
/// source. The files of std and of dependencies keep absolute paths.
fn own_file(file: &str) -> bool {
    file.starts_with("src/") || file.starts_with("src\\")
}

#[cfg(test)]
#[path = "crash_tests.rs"]
mod tests;
