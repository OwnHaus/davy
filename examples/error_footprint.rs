//! Disk and writer time of the error events of hot issues, through the
//! real writer (`store`) or as the bare SQL of one way to keep payloads
//! (`sim`), see "Payloads of hot issues" in `docs/design.md`.
//!
//! ```sh
//! cargo run --release --example error_footprint -- store DATA ISSUES PER_ISSUE PER_TX
//! cargo run --release --example error_footprint -- sim DESIGN DATA ISSUES PER_ISSUE PER_TX
//! cargo run --release --example error_footprint -- reads OLD NEW ROUNDS
//! cargo run --release --example error_footprint -- upgrade DATA
//! ```
//!
//! Event `i` belongs to issue `i % ISSUES`. `PER_TX` events go into one
//! writer transaction. The run ingests 14 days of events, then twice
//! expires the oldest 7 days and ingests 7 more, so the last numbers are
//! those of a store that has run past its retention, written into the
//! pages expiry freed. `DATA` must not exist. Each line of output is
//! `key=value`.
//!
//! `reads` times the payload reads of the store `OLD`, written before
//! `latest_payloads`, against those of `NEW`, in turns in one process, so
//! the load of a shared machine falls on both. It prints the median of
//! `ROUNDS` rounds. `upgrade` migrates the store `DATA` in place and times
//! the migrations that run. No older Davy opens the store after that, so
//! `upgrade` takes only a copy: a `DATA` under a `.scratch` directory, with
//! no link to a database outside one. That guards against a slip, not a
//! store that is named so. The counts must be above 0.
use anyhow::{Context as _, Result, bail, ensure};
use davy::{envelope::Envelope, event::ErrorEvent, http::body::Budget, retention::Days, store};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, time::Instant};

#[path = "error_footprint/reads.rs"]
mod reads;

const DAY_MS: i64 = 86_400_000;
/// When the first event arrives: 2026-11-01, so runs are comparable.
const START: i64 = 1_793_491_200_000;
/// Marked a payload kept only for being the newest, see the migration in
/// `src/store/latest.rs`.
const LATEST: [u8; 8] = [0x5D, 0x2A, 0x4D, 0x18, 0, 0, 0, 0];
/// The most events a run writes, far past any run that fits a disk, so
/// the counts, ids and times it computes from them cannot overflow.
const MAX_EVENTS: usize = 1_000_000_000;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("store") => run(None, &args[1..]),
        Some("sim") if args.len() > 1 => run(Some(Design::parse(&args[1])?), &args[2..]),
        Some("reads") => reads::compare(&args[1..]),
        Some("upgrade") if args.len() == 2 => upgrade(Path::new(&args[1])),
        _ => bail!(
            "usage: error_footprint store DATA ISSUES PER_ISSUE PER_TX\n       error_footprint sim current|table|side|autovacuum DATA ISSUES PER_ISSUE PER_TX\n       error_footprint reads OLD NEW ROUNDS\n       error_footprint upgrade DATA"
        ),
    }
}

/// Where a design keeps the payload of an issue's newest event.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Design {
    /// In its `events` row, marked, and cleared by the next event, as Davy
    /// did before `latest_payloads`.
    Current,
    /// Every payload in a `payloads` table keyed by event, deleted by the
    /// next event.
    Table,
    /// Sampled payloads in `events`, the newest in a table keyed by issue.
    Side,
    /// [`Design::Current`] with `auto_vacuum=INCREMENTAL`.
    AutoVacuum,
}

impl Design {
    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "current" => Self::Current,
            "table" => Self::Table,
            "side" => Self::Side,
            "autovacuum" => Self::AutoVacuum,
            _ => bail!("unknown design {s:?}"),
        })
    }
}

struct Run {
    issues: usize,
    per_issue: usize,
    per_tx: usize,
    event: Value,
}

impl Run {
    fn events(&self) -> usize {
        self.issues * self.per_issue
    }

    /// Event `i` of the run, received at `at`.
    fn event(&self, i: usize, at: i64) -> Result<ErrorEvent> {
        let mut raw = self.event.clone();
        raw["event_id"] = format!("{:032x}", i + 1).into();
        raw["timestamp"] = json!(at / 1000);
        raw["fingerprint"] = json!([format!("hot-{}", i % self.issues)]);
        raw["release"] = format!("app@1.0.{}", i % 3).into();
        raw["environment"] = if i.is_multiple_of(10) {
            "staging"
        } else {
            "production"
        }
        .into();
        let mut e = ErrorEvent::parse(raw)?;
        e.received_at = at;
        e.timestamp = at;
        Ok(e)
    }

    /// The receive time of event `i`: the run's events fill 14 days.
    fn at(&self, i: usize) -> Result<i64> {
        let n = i64::try_from(self.events())?;
        Ok(START + 14 * DAY_MS * i64::try_from(i)? / n)
    }
}

/// The data directory and the run that `args` ask for, checked before
/// anything is written, so a test can see a refusal without a run.
fn parse_run(args: &[String]) -> Result<(&Path, Run)> {
    let [data, issues, per_issue, per_tx] = args else {
        bail!("expected DATA ISSUES PER_ISSUE PER_TX");
    };
    let data = Path::new(data);
    ensure!(!data.exists(), "{} exists", data.display());
    let body = include_bytes!("../e2e/fixtures/sentry-node-10/4.body");
    let envelope = Envelope::parse(body).map_err(anyhow::Error::msg)?;
    let r = Run {
        issues: count("ISSUES", issues)?,
        per_issue: count("PER_ISSUE", per_issue)?,
        per_tx: count("PER_TX", per_tx)?,
        event: serde_json::from_slice(envelope.items[0].payload)?,
    };
    ensure!(
        r.issues
            .checked_mul(r.per_issue)
            .is_some_and(|n| n <= MAX_EVENTS),
        "ISSUES times PER_ISSUE must be at most {MAX_EVENTS}"
    );
    Ok((data, r))
}

fn run(design: Option<Design>, args: &[String]) -> Result<()> {
    let (data, r) = parse_run(args)?;
    let mut target: Box<dyn Target> = match design {
        None => Box::new(Real::open(data)?),
        Some(d) => Box::new(Sim::open(data, d, r.issues)?),
    };
    let n = r.events();
    let mut written = ingest(&r, target.as_mut(), 0..n)?;
    report("ingested", data, n)?;
    for cycle in 1..=2 {
        target.expire(r.at(cycle * n / 2)?)?;
        written += ingest(
            &r,
            target.as_mut(),
            (cycle + 1) * n / 2..(cycle + 2) * n / 2,
        )?;
        report(&format!("cycle{cycle}"), data, n)?;
    }
    out(
        "writer_us_per_event",
        format!("{:.1}", written * 1e6 / ratio(2 * n)),
    )?;
    target.reads()?;
    let floor = data.join("compacted.sqlite");
    Connection::open(data.join(store::DB_FILE))?
        .execute("VACUUM INTO ?1", [floor.to_string_lossy()])?;
    out(
        "compacted_bytes_per_event",
        format!("{:.0}", ratio(len(&floor)?) / ratio(n)),
    )
}

fn upgrade(data: &Path) -> Result<()> {
    ensure!(data.exists(), "{} does not exist", data.display());
    // SQLite follows a link to the database or its WAL, so each file is
    // judged where it really lies.
    let mut real = vec![data.canonicalize()?];
    for suffix in ["", "-wal", "-shm"] {
        let mut name = data.join(store::DB_FILE).into_os_string();
        name.push(suffix);
        let file = std::path::PathBuf::from(name);
        if file.symlink_metadata().is_ok() {
            ensure!(
                links(&file)? == 1,
                "{} has another hard link, so upgrade would migrate the store it shares. Copy the file, do not link it",
                file.display()
            );
            real.push(file.canonicalize()?);
        }
    }
    ensure!(
        real.iter().all(|p| is_copy(p)),
        "upgrade migrates {} in place, and no older Davy opens it after. Copy the data directory under a .scratch directory and pass the copy",
        data.display()
    );
    let started = Instant::now();
    let c = store::open(data)?;
    out(
        "upgrade_secs",
        format!("{:.2}", started.elapsed().as_secs_f64()),
    )?;
    let moved: i64 = c.query_row("SELECT count(*) FROM latest_payloads", [], |r| r.get(0))?;
    out("latest_payloads", moved)
}

/// Whether `path` lies under a `.scratch` directory, where only a copy of
/// a store would be.
fn is_copy(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".scratch")
}

/// The hard links of the file `path`, 1 where the platform does not say.
fn links(path: &Path) -> Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(std::fs::metadata(path)?.nlink())
    }
    #[cfg(not(unix))]
    {
        drop(std::fs::metadata(path)?);
        Ok(1)
    }
}

/// The count `name` of the command line, which a run divides by or steps
/// with, so 0 is refused.
fn count(name: &str, value: &str) -> Result<usize> {
    let n: usize = value
        .parse()
        .with_context(|| format!("{name} must be a whole number above 0, not {value}"))?;
    ensure!(n > 0, "{name} must be above 0");
    Ok(n)
}

/// Writes events `ids`, `per_tx` in a transaction, and returns the seconds
/// the writer took.
fn ingest(r: &Run, target: &mut dyn Target, ids: std::ops::Range<usize>) -> Result<f64> {
    let mut secs = 0.0;
    let ids: Vec<usize> = ids.collect();
    for chunk in ids.chunks(r.per_tx) {
        let events = chunk
            .iter()
            .map(|&i| r.event(i, r.at(i)?))
            .collect::<Result<Vec<_>>>()?;
        let started = Instant::now();
        target.write(events)?;
        secs += started.elapsed().as_secs_f64();
    }
    Ok(secs)
}

trait Target {
    fn write(&mut self, events: Vec<ErrorEvent>) -> Result<()>;
    /// Deletes the events received before `before`.
    fn expire(&mut self, before: i64) -> Result<()>;
    /// Times the reads of payloads.
    fn reads(&mut self) -> Result<()>;
}

/// The writer thread of `davy serve`.
struct Real {
    writer: store::Writer,
    c: Connection,
    project: i64,
    budget: Budget,
}

impl Real {
    fn open(data: &Path) -> Result<Self> {
        let c = store::open(data)?;
        let (project, _) = store::create_project(&c, "bench")?;
        Ok(Self {
            writer: store::Writer::start(store::open(data)?, 1024),
            c,
            project,
            budget: Budget::new(1 << 20),
        })
    }

    fn send(&self, job: store::Job) -> Result<()> {
        let (done, wait) = tokio::sync::oneshot::channel();
        self.writer.sender.send(store::Write { job, done })?;
        wait.blocking_recv()?.map_err(anyhow::Error::msg)
    }
}

impl Target for Real {
    fn write(&mut self, events: Vec<ErrorEvent>) -> Result<()> {
        self.send(store::Job::Ingest {
            project: self.project,
            events,
            held: self.budget.hold(),
        })
    }

    fn expire(&mut self, before: i64) -> Result<()> {
        let count = |c: &Connection| -> Result<i64> {
            Ok(c.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?)
        };
        let started = Instant::now();
        let mut left = count(&self.c)?;
        loop {
            self.send(store::Job::Sweep {
                at: before + DAY_MS,
                error_days: Days::new(1).map_err(anyhow::Error::msg)?,
            })?;
            let now = count(&self.c)?;
            if now == left {
                break;
            }
            left = now;
        }
        out(
            "expire_secs",
            format!("{:.2}", started.elapsed().as_secs_f64()),
        )
    }

    fn reads(&mut self) -> Result<()> {
        let issues: Vec<i64> = self
            .c
            .prepare("SELECT id FROM issues")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let reps = 200;
        let time = |f: &dyn Fn(i64) -> Result<()>| -> Result<String> {
            let started = Instant::now();
            for _ in 0..reps {
                for &issue in &issues {
                    f(issue)?;
                }
            }
            Ok(format!(
                "{:.1}",
                started.elapsed().as_secs_f64() * 1e6 / ratio(reps * issues.len())
            ))
        };
        let c = &self.c;
        out("read_latest_us", time(&|i| store::payload(c, i).map(drop))?)?;
        out(
            "read_stored_us",
            time(&|i| davy::issues::stored_payload(c, i, None).map(drop))?,
        )?;
        out(
            "read_events_page_us",
            time(&|i| davy::issues::events(c, i, None).map(drop))?,
        )?;
        let count = |sql| -> Result<i64> { Ok(c.query_row(sql, [], |r| r.get(0))?) };
        out(
            "payloads_in_events",
            count("SELECT count(payload) FROM events")?,
        )?;
        out(
            "latest_payloads",
            count("SELECT count(*) FROM latest_payloads")?,
        )
    }
}

/// The bare SQL of one design, on Davy's schema.
struct Sim {
    c: Connection,
    design: Design,
    /// Events written per issue, as `issues.event_count` counts them.
    counts: Vec<i64>,
}

impl Sim {
    fn open(data: &Path, design: Design, issues: usize) -> Result<Self> {
        // Readable by its owner only, as `store::open` makes a data
        // directory.
        let mut dir = std::fs::DirBuilder::new();
        dir.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
        dir.create(data)?;
        let mut c = Connection::open(data.join(store::DB_FILE))?;
        if design == Design::AutoVacuum {
            c.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        }
        store::init(&mut c)?;
        c.execute_batch(
            "INSERT INTO projects(id,slug,public_key,created_at) VALUES (1,'bench','k',0);
             CREATE TABLE payloads(event INTEGER PRIMARY KEY, payload BLOB NOT NULL);",
        )?;
        for i in 1..=issues {
            c.execute(
                "INSERT INTO issues(id,project_id,grouping_version,fingerprint,title,level,status,first_seen,last_seen,event_count) VALUES (?1,1,1,?1,'t','error','unresolved',0,0,0)",
                [i64::try_from(i)?],
            )?;
        }
        Ok(Self {
            c,
            design,
            counts: vec![0; issues],
        })
    }

    /// Clears the payload the issue's previous event kept for being newest.
    fn clear_previous(&self, issue: i64) -> Result<()> {
        let sql = match self.design {
            Design::Current | Design::AutoVacuum => {
                "UPDATE events SET payload=NULL WHERE id=(SELECT max(id) FROM events WHERE issue_id=?1) AND substr(payload,1,8)=?2"
            }
            Design::Table => {
                "DELETE FROM payloads WHERE event=(SELECT max(id) FROM events WHERE issue_id=?1) AND substr(payload,1,8)=?2"
            }
            Design::Side => return Ok(()),
        };
        self.c
            .prepare_cached(sql)?
            .execute(params![issue, LATEST.as_slice()])?;
        Ok(())
    }

    fn insert(&self, issue: i64, e: &ErrorEvent, payload: Option<Vec<u8>>) -> Result<i64> {
        let inline = if self.design == Design::Table {
            None
        } else {
            payload.clone()
        };
        self.c
            .prepare_cached("INSERT INTO events(project_id,issue_id,event_id,timestamp,received_at,level,release,environment,trace_id,user_id,payload) VALUES (1,?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)")?
            .execute(params![issue, e.event_id.0, e.timestamp, e.received_at, e.level.as_str(), e.release, e.environment, e.trace_id, e.user_id, inline])?;
        let id = self.c.last_insert_rowid();
        if let (Design::Table, Some(p)) = (self.design, payload) {
            self.c
                .prepare_cached("INSERT INTO payloads(event,payload) VALUES (?1,?2)")?
                .execute(params![id, p])?;
        }
        Ok(id)
    }

    /// Keeps the payload of an issue's newest event, outside the sample.
    fn keep_newest(&self, issue: i64, event: i64, e: &ErrorEvent) -> Result<()> {
        let payload = compress(e)?;
        let marked = [LATEST.as_slice(), &payload].concat();
        let c = &self.c;
        match self.design {
            Design::Current | Design::AutoVacuum => c
                .prepare_cached("UPDATE events SET payload=?2 WHERE id=?1")?
                .execute(params![event, marked])?,
            Design::Table => c
                .prepare_cached("INSERT INTO payloads(event,payload) VALUES (?1,?2)")?
                .execute(params![event, marked])?,
            Design::Side => c
                .prepare_cached("INSERT INTO latest_payloads(issue_id,event,payload) VALUES (?1,?2,?3) ON CONFLICT(issue_id) DO UPDATE SET event=excluded.event,payload=excluded.payload")?
                .execute(params![issue, event, payload])?,
        };
        Ok(())
    }
}

impl Target for Sim {
    fn write(&mut self, events: Vec<ErrorEvent>) -> Result<()> {
        let tx = self.c.unchecked_transaction()?;
        let mut newest: HashMap<i64, Option<(i64, &ErrorEvent)>> = HashMap::new();
        for e in &events {
            let fingerprint = e
                .fingerprint
                .as_ref()
                .and_then(|f| f.first())
                .map_or("", String::as_str);
            let slot: usize = fingerprint.trim_start_matches("hot-").parse()?;
            let issue = i64::try_from(slot + 1)?;
            let n = self.counts[slot];
            if n > 0 && !newest.contains_key(&issue) {
                self.clear_previous(issue)?;
            }
            let kept = sampled(n);
            let id = self.insert(issue, e, kept.then(|| compress(e)).transpose()?)?;
            self.counts[slot] += 1;
            newest.insert(issue, (!kept).then_some((id, e)));
        }
        for (issue, latest) in newest {
            match latest {
                Some((event, e)) => self.keep_newest(issue, event, e)?,
                None if self.design == Design::Side => {
                    tx.prepare_cached("DELETE FROM latest_payloads WHERE issue_id=?1")?
                        .execute([issue])?;
                }
                None => {}
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn expire(&mut self, before: i64) -> Result<()> {
        let started = Instant::now();
        let tx = self.c.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM payloads WHERE event IN (SELECT id FROM events WHERE received_at<?1)",
            [before],
        )?;
        tx.execute("DELETE FROM events WHERE received_at<?1", [before])?;
        tx.commit()?;
        if self.design == Design::AutoVacuum {
            self.c.execute_batch("PRAGMA incremental_vacuum")?;
        }
        out(
            "expire_secs",
            format!("{:.2}", started.elapsed().as_secs_f64()),
        )
    }

    fn reads(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Prints the size of the store in `data` per live event.
fn report(phase: &str, data: &Path, live: usize) -> Result<()> {
    let c = Connection::open(data.join(store::DB_FILE))?;
    c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let free: i64 = c.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    let size: i64 = c.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    out(&format!("{phase}_bytes"), pages * size)?;
    out(&format!("{phase}_free_pages"), free)?;
    out(
        &format!("{phase}_bytes_per_event"),
        format!("{:.0}", ratio(usize::try_from(pages * size)?) / ratio(live)),
    )
}

/// As `src/store/latest.rs`.
fn sampled(n: i64) -> bool {
    n < 50 || (n + 1) % 100 == 0
}

/// As `src/store/latest.rs`.
fn compress(e: &ErrorEvent) -> Result<Vec<u8>> {
    Ok(zstd::stream::encode_all(
        serde_json::to_vec(&e.raw)?.as_slice(),
        1,
    )?)
}

fn len(path: &Path) -> Result<usize> {
    Ok(usize::try_from(std::fs::metadata(path)?.len())?)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "counts in a benchmark stay far below 2^52"
)]
fn ratio(n: usize) -> f64 {
    n as f64
}

fn out(key: &str, value: impl std::fmt::Display) -> Result<()> {
    use std::io::Write as _;
    writeln!(std::io::stdout(), "{key}={value}")?;
    Ok(())
}

#[cfg(test)]
#[path = "error_footprint/tests.rs"]
mod tests;
