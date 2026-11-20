use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

const T0: i64 = 1_763_600_000_000;

struct Row {
    dt: i64,
    level: u8,
    service: &'static str,
    message: String,
    attrs: String,
}

struct Gen {
    rng: SmallRng,
    i: u64,
    dt: i64,
}

const SERVICES: &[&str] = &["api", "api-worker", "cobra", "ai", "api-cron", "api-public", "api-admin"];
const RESOURCES: &[&str] = &["events", "organizers", "users", "venues", "tickets", "reports", "webhooks", "sessions", "templates", "uploads", "devices", "settings"];
const CONTEXTS: &[&str] = &["EventService", "UserService", "MailService", "CacheService", "SyncService", "WebhookService", "ReportService", "AuthGuard", "UploadService", "CronService"];
const UAS: &[&str] = &[
    "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0 Safari/537.36",
    "Dart/3.9 (dart:io)",
    "okhttp/4.12.0",
    "node-fetch/1.0 (+https://github.com/bitinn/node-fetch)",
];
const ERRORS: &[(&str, &str)] = &[
    ("TimeoutError", "timeout of 5000ms exceeded"),
    ("Error", "connect ECONNREFUSED 10.0.1.5:5432"),
    ("TypeError", "Cannot read properties of undefined (reading 'id')"),
    ("QueryFailedError", "duplicate key value violates unique constraint \"uq_session_token\""),
    ("NotFoundException", "Entity not found"),
    ("Error", "Redis connection lost, reconnecting"),
    ("AxiosError", "Request failed with status code 502"),
];
const FRAMES: &[&str] = &[
    "at EventService.findOne (/app/dist/modules/event/event.service.js:142:23)",
    "at async EventController.get (/app/dist/modules/event/event.controller.js:58:20)",
    "at async /app/node_modules/@nestjs/core/router/router-execution-context.js:46:28",
    "at async UserService.resolve (/app/dist/modules/user/user.service.js:77:11)",
    "at Socket.emit (node:events:519:28)",
    "at TCPConnectWrap.afterConnect [as oncomplete] (node:net:1611:16)",
    "at QueryRunner.query (/app/node_modules/typeorm/driver/postgres/PostgresQueryRunner.js:219:19)",
    "at processTicksAndRejections (node:internal/process/task_queues:105:5)",
];

impl Gen {
    fn new(seed: u64) -> Self {
        Gen { rng: SmallRng::seed_from_u64(seed), i: 0, dt: T0 }
    }

    fn hex(&mut self, n: usize, s: &mut String) {
        for _ in 0..n {
            let v: u8 = self.rng.gen_range(0..16);
            s.push(char::from_digit(v as u32, 16).unwrap());
        }
    }

    fn uuid(&mut self) -> String {
        let mut s = String::with_capacity(36);
        self.hex(8, &mut s);
        s.push('-');
        self.hex(4, &mut s);
        s.push('-');
        self.hex(4, &mut s);
        s.push('-');
        self.hex(4, &mut s);
        s.push('-');
        self.hex(12, &mut s);
        s
    }

    fn next(&mut self) -> (Row, String) {
        self.i += 1;
        self.dt += self.rng.gen_range(0..18);
        let pod = self.rng.gen_range(0..24);
        let pid = 1000 + pod * 7;
        let service = SERVICES[self.rng.gen_range(0..SERVICES.len())];
        let roll: u32 = self.rng.gen_range(0..1000);
        let req_id = self.uuid();
        let mut trace = String::with_capacity(32);
        self.hex(32, &mut trace);
        let mut attrs = String::with_capacity(900);
        let level;
        let message;
        if roll < 700 {
            level = 30;
            message = "request completed".to_string();
            let method = ["GET", "GET", "GET", "POST", "PUT", "DELETE"][self.rng.gen_range(0..6)];
            let res = RESOURCES[self.rng.gen_range(0..RESOURCES.len())];
            let id: u32 = self.rng.gen_range(1..2_000_000);
            let status = [200, 200, 200, 200, 201, 204, 304, 400, 401, 404, 500][self.rng.gen_range(0..11)];
            let rt: u32 = self.rng.gen_range(1..900);
            let ua = UAS[self.rng.gen_range(0..UAS.len())];
            let page: u32 = self.rng.gen_range(1..40);
            write!(
                attrs,
                r#"{{"pid":{pid},"hostname":"{service}-7d9f{pod:02}-x{pod}k","context":"HTTP","traceId":"{trace}","req":{{"id":"{req_id}","method":"{method}","url":"/v1/{res}/{id}?page={page}&limit=50","remoteAddress":"10.0.{}.{}","headers":{{"user-agent":"{ua}","x-request-id":"{req_id}"}}}},"res":{{"statusCode":{status}}},"responseTime":{rt}}}"#,
                self.rng.gen_range(0..8),
                self.rng.gen_range(2..250)
            )
            .unwrap();
        } else if roll < 900 {
            level = 30;
            let ctx = CONTEXTS[self.rng.gen_range(0..CONTEXTS.len())];
            let uid: u32 = self.rng.gen_range(1..800_000);
            let n: u32 = self.rng.gen_range(1..5000);
            message = match self.rng.gen_range(0..6) {
                0 => format!("Event {n} synced to cache in {} ms", n % 300),
                1 => format!("User {uid} logged in via password"),
                2 => format!("Sent email order-confirmation to user {uid}"),
                3 => format!("Cron job cleanup-sessions finished in {n} ms, removed {} rows", n / 3),
                4 => format!("Webhook {} delivered to partner endpoint", self.uuid()),
                _ => format!("Report {n} generated for organizer {uid}"),
            };
            write!(attrs, r#"{{"pid":{pid},"hostname":"{service}-7d9f{pod:02}-x{pod}k","context":"{ctx}","traceId":"{trace}","userId":{uid}}}"#).unwrap();
        } else if roll < 970 {
            level = 40;
            let n: u32 = self.rng.gen_range(500..9000);
            message = if self.rng.gen_bool(0.5) {
                format!("Slow query took {n} ms")
            } else {
                format!("Retrying request to partner-api.internal attempt {}", n % 5)
            };
            write!(attrs, r#"{{"pid":{pid},"hostname":"{service}-7d9f{pod:02}-x{pod}k","context":"TypeOrm","traceId":"{trace}","query":"SELECT * FROM event WHERE organizer_id = $1 ORDER BY starts_at DESC LIMIT 50"}}"#).unwrap();
        } else {
            level = 50;
            let (ty, msg) = ERRORS[self.rng.gen_range(0..ERRORS.len())];
            message = format!("Unhandled exception: {ty}: {msg}");
            let mut stack = format!("{ty}: {msg}");
            let frames = self.rng.gen_range(5..10);
            for _ in 0..frames {
                stack.push_str("\\n    ");
                stack.push_str(FRAMES[self.rng.gen_range(0..FRAMES.len())]);
            }
            write!(attrs, r#"{{"pid":{pid},"hostname":"{service}-7d9f{pod:02}-x{pod}k","context":"ExceptionsHandler","traceId":"{trace}","req":{{"id":"{req_id}"}},"err":{{"type":"{ty}","message":"{}","stack":"{stack}"}}}}"#, msg.replace('"', "\\\"")).unwrap();
        }
        (Row { dt: self.dt, level, service, message, attrs }, trace)
    }
}

fn dir_size(p: &Path) -> u64 {
    let mut t = 0;
    for e in std::fs::read_dir(p).unwrap() {
        let e = e.unwrap();
        let m = e.metadata().unwrap();
        t += if m.is_dir() { dir_size(&e.path()) } else { m.len() };
    }
    t
}

fn max_rss_mb() -> f64 {
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut u);
        u.ru_maxrss as f64 / 1024.0 / 1024.0
    }
}

fn cpu_secs() -> f64 {
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut u);
        let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        tv(u.ru_utime) + tv(u.ru_stime)
    }
}

trait Store {
    fn append(&mut self, rows: &[Row]);
    fn finish(&mut self);
}

struct Sqlite {
    conn: rusqlite::Connection,
    fts: bool,
}

impl Sqlite {
    fn open(dir: &Path, fts: Option<&str>) -> Self {
        Self::open_opts(dir, fts, "")
    }
    fn open_opts(dir: &Path, fts: Option<&str>, extra: &str) -> Self {
        let conn = rusqlite::Connection::open(dir.join("day.db")).unwrap();
        conn.execute_batch(
            "PRAGMA page_size=8192; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-262144; PRAGMA temp_store=MEMORY;
             CREATE TABLE logs(id INTEGER PRIMARY KEY, dt INTEGER NOT NULL, level INTEGER NOT NULL, service TEXT NOT NULL, message TEXT NOT NULL, attrs TEXT NOT NULL);
             CREATE INDEX logs_dt ON logs(dt);",
        )
        .unwrap();
        if let Some(tok) = fts {
            conn.execute_batch(&format!(
                "CREATE VIRTUAL TABLE logs_fts USING fts5(message, attrs, content='logs', content_rowid='id', tokenize='{tok}'{extra});"
            ))
            .unwrap();
        }
        Sqlite { conn, fts: fts.is_some() }
    }
}

impl Store for Sqlite {
    fn append(&mut self, rows: &[Row]) {
        let tx = self.conn.transaction().unwrap();
        {
            let mut ins = tx.prepare_cached("INSERT INTO logs(dt, level, service, message, attrs) VALUES (?1, ?2, ?3, ?4, ?5)").unwrap();
            let mut fts = if self.fts { Some(tx.prepare_cached("INSERT INTO logs_fts(rowid, message, attrs) VALUES (?1, ?2, ?3)").unwrap()) } else { None };
            for r in rows {
                ins.execute(rusqlite::params![r.dt, r.level, r.service, r.message, r.attrs]).unwrap();
                if let Some(f) = fts.as_mut() {
                    let id = tx.last_insert_rowid();
                    f.execute(rusqlite::params![id, r.message, r.attrs]).unwrap();
                }
            }
        }
        tx.commit().unwrap();
    }
    fn finish(&mut self) {
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    }
}

struct Tant {
    writer: Option<tantivy::IndexWriter>,
    f: TantFields,
    since_commit: usize,
}

#[derive(Clone, Copy)]
struct TantFields {
    dt: tantivy::schema::Field,
    level: tantivy::schema::Field,
    service: tantivy::schema::Field,
    message: tantivy::schema::Field,
    attrs: tantivy::schema::Field,
}

fn tant_schema() -> (tantivy::schema::Schema, TantFields) {
    use tantivy::schema::*;
    let mut b = Schema::builder();
    let dt = b.add_i64_field("dt", INDEXED | FAST | STORED);
    let level = b.add_u64_field("level", INDEXED | FAST | STORED);
    let service = b.add_text_field("service", STRING | STORED);
    let message = b.add_text_field("message", TEXT | STORED);
    let attrs = b.add_text_field("attrs", TEXT | STORED);
    (b.build(), TantFields { dt, level, service, message, attrs })
}

impl Tant {
    fn open(dir: &Path, threads: usize) -> Self {
        let (schema, f) = tant_schema();
        let mut idx = tantivy::Index::builder().schema(schema);
        let settings = tantivy::IndexSettings {
            docstore_compression: tantivy::store::Compressor::Zstd(tantivy::store::ZstdCompressor::default()),
            ..Default::default()
        };
        idx = idx.settings(settings);
        let index = idx.create_in_dir(dir).unwrap();
        let writer = index.writer_with_num_threads(threads, 256_000_000).unwrap();
        Tant { writer: Some(writer), f, since_commit: 0 }
    }
}

impl Store for Tant {
    fn append(&mut self, rows: &[Row]) {
        for r in rows {
            let mut d = tantivy::TantivyDocument::default();
            d.add_i64(self.f.dt, r.dt);
            d.add_u64(self.f.level, r.level as u64);
            d.add_text(self.f.service, r.service);
            d.add_text(self.f.message, &r.message);
            d.add_text(self.f.attrs, &r.attrs);
            self.writer.as_ref().unwrap().add_document(d).unwrap();
        }
        self.since_commit += rows.len();
        if self.since_commit >= 100_000 {
            self.writer.as_mut().unwrap().commit().unwrap();
            self.since_commit = 0;
        }
    }
    fn finish(&mut self) {
        let mut w = self.writer.take().unwrap();
        w.commit().unwrap();
        w.wait_merging_threads().unwrap();
    }
}


struct ZstdSeg {
    out: std::io::BufWriter<std::fs::File>,
    buf: Vec<u8>,
}

impl Store for ZstdSeg {
    fn append(&mut self, rows: &[Row]) {
        use std::io::Write;
        for r in rows {
            writeln!(self.buf, "{}\t{}\t{}\t{}\t{}", r.dt, r.level, r.service, r.message, r.attrs).unwrap();
        }
        if self.buf.len() > 4 << 20 {
            let c = zstd::bulk::compress(&self.buf, 3).unwrap();
            self.out.write_all(&(c.len() as u32).to_le_bytes()).unwrap();
            self.out.write_all(&c).unwrap();
            self.buf.clear();
        }
    }
    fn finish(&mut self) {
        use std::io::Write;
        if !self.buf.is_empty() {
            let c = zstd::bulk::compress(&self.buf, 3).unwrap();
            self.out.write_all(&(c.len() as u32).to_le_bytes()).unwrap();
            self.out.write_all(&c).unwrap();
            self.buf.clear();
        }
        self.out.flush().unwrap();
        self.out.get_ref().sync_all().unwrap();
    }
}

fn open_store(variant: &str, dir: &Path) -> Box<dyn Store> {
    match variant {
        "sqlite" => Box::new(Sqlite::open(dir, None)),
        "sqlite-fts-word" => Box::new(Sqlite::open(dir, Some("unicode61"))),
        "sqlite-fts-tri" => Box::new(Sqlite::open(dir, Some("trigram"))),
        "sqlite-fts-word-none" => Box::new(Sqlite::open_opts(dir, Some("unicode61"), ", detail=none")),
        "tantivy1" => Box::new(Tant::open(dir, 1)),
        "tantivy4" => Box::new(Tant::open(dir, 4)),
        "zstd" => Box::new(ZstdSeg { out: std::io::BufWriter::new(std::fs::File::create(dir.join("seg.zst")).unwrap()), buf: Vec::new() }),
        _ => panic!("unknown variant {variant}"),
    }
}

fn ingest(variant: &str, dir: &Path, total: usize, batch: usize) {
    if dir.exists() {
        std::fs::remove_dir_all(dir).unwrap();
    }
    std::fs::create_dir_all(dir).unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<Row>>(8);
    let needle_at = total / 2;
    let dirc = dir.to_path_buf();
    let producer = std::thread::spawn(move || {
        let mut g = Gen::new(42);
        let mut raw_bytes = 0u64;
        let mut sent = 0;
        while sent < total {
            let n = batch.min(total - sent);
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                let (r, id) = g.next();
                if sent + v.len() == needle_at {
                    std::fs::write(dirc.join("needle.txt"), &id).unwrap();
                }
                raw_bytes += (r.message.len() + r.attrs.len() + r.service.len() + 20) as u64;
                v.push(r);
            }
            sent += n;
            if tx.send(v).is_err() {
                break;
            }
        }
        raw_bytes
    });
    let mut store = open_store(variant, dir);
    let start = Instant::now();
    let cpu0 = cpu_secs();
    let mut done = 0usize;
    let mut mark = Instant::now();
    let mut per_million = Vec::new();
    for v in rx {
        store.append(&v);
        done += v.len();
        if done % 1_000_000 == 0 {
            per_million.push(1_000_000.0 / mark.elapsed().as_secs_f64());
            mark = Instant::now();
        }
    }
    store.finish();
    let secs = start.elapsed().as_secs_f64();
    let cpu = cpu_secs() - cpu0;
    let raw = producer.join().unwrap();
    let disk = dir_size(dir);
    let pm: Vec<String> = per_million.iter().map(|r| format!("{:.0}", r)).collect();
    println!(
        "ingest variant={variant} rows={done} secs={secs:.2} rows_per_s={:.0} cpu_s={cpu:.1} raw_mb={:.0} disk_mb={:.0} disk_over_raw={:.2} max_rss_mb={:.0} per_million=[{}]",
        done as f64 / secs,
        raw as f64 / 1e6,
        disk as f64 / 1e6,
        disk as f64 / raw as f64,
        max_rss_mb(),
        pm.join(",")
    );
}

fn gen_check(total: usize) {
    let mut g = Gen::new(42);
    let start = Instant::now();
    let mut bytes = 0usize;
    for _ in 0..total {
        let (r, _) = g.next();
        bytes += r.message.len() + r.attrs.len();
    }
    let s = start.elapsed().as_secs_f64();
    println!("gen rows={total} rows_per_s={:.0} avg_bytes={}", total as f64 / s, bytes / total);
}

fn median(mut v: Vec<f64>) -> (f64, f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (v[v.len() / 2], v[0], v[v.len() - 1])
}

fn time5<F: FnMut() -> usize>(name: &str, mut f: F) {
    let mut ts = Vec::new();
    let mut hits = 0;
    for _ in 0..5 {
        let s = Instant::now();
        hits = f();
        ts.push(s.elapsed().as_secs_f64() * 1000.0);
    }
    let (m, lo, hi) = median(ts);
    println!("  {name}: median_ms={m:.1} range=[{lo:.1}..{hi:.1}] hits={hits}");
}

fn query(variant: &str, dir: &Path) {
    let needle = std::fs::read_to_string(dir.join("needle.txt")).unwrap();
    println!("query variant={variant} needle={needle}");
    if variant.starts_with("sqlite") {
        let conn = rusqlite::Connection::open(dir.join("day.db")).unwrap();
        conn.execute_batch("PRAGMA cache_size=-262144;").unwrap();
        let max_dt: i64 = conn.query_row("SELECT max(dt) FROM logs", [], |r| r.get(0)).unwrap();
        let count = |sql: &str, p: &[&dyn rusqlite::ToSql]| -> usize {
            let mut st = conn.prepare_cached(sql).unwrap();
            let rows = st.query_map(p, |r| r.get::<_, i64>(0)).unwrap();
            rows.map(|r| r.unwrap()).count()
        };
        let fts_needle = if variant == "sqlite-fts-tri" { format!("\"{needle}\"") } else { format!("\"{}\"", needle.replace('-', " ")) };
        if variant != "sqlite" {
            time5("needle_trace_id", || count("SELECT rowid FROM logs_fts WHERE logs_fts MATCH ?1", &[&fts_needle]));
            time5("term_timeout_latest100", || {
                count("SELECT rowid FROM logs_fts WHERE logs_fts MATCH 'message:timeout' ORDER BY rowid DESC LIMIT 100", &[])
            });
            time5("term_timeout_count", || {
                let mut st = conn.prepare_cached("SELECT count(*) FROM logs_fts WHERE logs_fts MATCH 'message:timeout'").unwrap();
                st.query_row([], |r| r.get::<_, i64>(0)).unwrap() as usize
            });
        } else {
            time5("needle_trace_id_like_scan", || count("SELECT id FROM logs WHERE attrs LIKE ?1", &[&format!("%{needle}%")]));
        }
        time5("errors_last_hour_latest100", || {
            count("SELECT id FROM logs WHERE dt > ?1 AND level >= 50 ORDER BY dt DESC LIMIT 100", &[&(max_dt - 3_600_000)])
        });
        time5("count_by_level_last_hour", || {
            count("SELECT level FROM logs WHERE dt > ?1 GROUP BY level", &[&(max_dt - 3_600_000)])
        });
    } else if variant.starts_with("tantivy") {
        use tantivy::collector::{Count, TopDocs};
        use tantivy::query::{BooleanQuery, Occur, Query, RangeQuery, TermQuery};
        use tantivy::schema::IndexRecordOption;
        use tantivy::{Order, Term};
        let index = tantivy::Index::open_in_dir(dir).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let (_, f) = tant_schema();
        println!("  segments={}", searcher.segment_readers().len());
        let max_dt = {
            let top = searcher
                .search(&tantivy::query::AllQuery, &TopDocs::with_limit(1).order_by_fast_field::<i64>("dt", Order::Desc))
                .unwrap();
            top[0].0.unwrap()
        };
        let nq = TermQuery::new(Term::from_field_text(f.attrs, &needle), IndexRecordOption::Basic);
        time5("needle_trace_id", || searcher.search(&nq, &Count).unwrap());
        let timeout = TermQuery::new(Term::from_field_text(f.message, "timeout"), IndexRecordOption::Basic);
        time5("term_timeout_latest100", || {
            searcher.search(&timeout, &TopDocs::with_limit(100).order_by_fast_field::<i64>("dt", Order::Desc)).unwrap().len()
        });
        time5("term_timeout_count", || searcher.search(&timeout, &Count).unwrap());
        let range = RangeQuery::new(
            std::ops::Bound::Excluded(Term::from_field_i64(f.dt, max_dt - 3_600_000)),
            std::ops::Bound::Unbounded,
        );
        time5("errors_last_hour_latest100", || {
            let q = BooleanQuery::new(vec![
                (Occur::Must, Box::new(range.clone()) as Box<dyn Query>),
                (Occur::Must, Box::new(TermQuery::new(Term::from_field_u64(f.level, 50), IndexRecordOption::Basic))),
            ]);
            searcher.search(&q, &TopDocs::with_limit(100).order_by_fast_field::<i64>("dt", Order::Desc)).unwrap().len()
        });
        time5("count_by_level_last_hour", || {
            let mut n = 0;
            for lvl in [30u64, 40, 50] {
                let q = BooleanQuery::new(vec![
                    (Occur::Must, Box::new(range.clone()) as Box<dyn Query>),
                    (Occur::Must, Box::new(TermQuery::new(Term::from_field_u64(f.level, lvl), IndexRecordOption::Basic))),
                ]);
                if searcher.search(&q, &Count).unwrap() > 0 {
                    n += 1;
                }
            }
            n
        });
    } else {
        time5("needle_full_decompress_scan", || {
            let data = std::fs::read(dir.join("seg.zst")).unwrap();
            let mut off = 0;
            let mut hits = 0;
            let nb = needle.as_bytes();
            while off < data.len() {
                let len = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
                let block = zstd::bulk::decompress(&data[off + 4..off + 4 + len], 64 << 20).unwrap();
                hits += block.windows(nb.len()).filter(|w| *w == nb).count();
                off += 4 + len;
            }
            hits
        });
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let base = PathBuf::from(std::env::var("BENCH_DIR").expect("BENCH_DIR"));
    match a[1].as_str() {
        "gen" => gen_check(a[2].parse().unwrap()),
        "ingest" => {
            let batch = a.get(4).map(|s| s.parse().unwrap()).unwrap_or(5000);
            ingest(&a[2], &base.join(&a[2]), a[3].parse().unwrap(), batch)
        }
        "query" => query(&a[2], &base.join(&a[2])),
        _ => panic!("usage: gen N | ingest VARIANT N [BATCH] | query VARIANT"),
    }
}
