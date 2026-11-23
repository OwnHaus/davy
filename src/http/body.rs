//! Reading request bodies within one memory budget.
//!
//! A request pays for its memory before it uses it: the bytes it receives,
//! the body they decompress to, the tree it parses, and the records it
//! builds. Every request pays from one [`Budget`]. A request that needs more
//! than the whole budget gets `413`. One that finds the budget spent by
//! others gets `429`, and its client sends it again later.

use super::{Failure, bad};
use crate::envelope::EnvelopeHeader;
use axum::{body::Body, http::StatusCode};
use brotli_decompressor::BrotliDecoderParameter;
use http_body_util::BodyExt;
use std::{
    io::{BufRead, BufReader, Cursor, Read},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

pub const COMPRESSED: usize = 20 * 1024 * 1024;
/// Davy keeps events and logs, each at most 1 MiB, and drops attachments, so
/// it needs less room than Sentry's 200 MiB.
pub const DECOMPRESSED: usize = 64 * 1024 * 1024;
pub const MAX_HEADER_LINE: usize = 64 * 1024;
/// How much of an envelope is read before its key is known.
pub const BEFORE_AUTH: usize = 128 * 1024;
/// A zstd frame may ask its decoder for a window of up to 8 MiB, the most
/// RFC 9659 requires of an HTTP decoder. zstd's own limit is 128 MiB.
const ZSTD_WINDOW_LOG: u32 = 23;
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// What all requests together may hold while they are read and parsed.
pub const MEMORY: usize = 256 * 1024 * 1024;
const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;
/// The first room of a buffer that grows.
const FIRST_ROOM: usize = 64 * KIB;

/// Memory shared by all requests, counted in KiB.
#[derive(Clone)]
pub struct Budget {
    permits: Arc<Semaphore>,
    kib: usize,
}

impl Budget {
    pub fn new(bytes: usize) -> Self {
        let kib = bytes / KIB;
        Self {
            permits: Arc::new(Semaphore::new(kib)),
            kib,
        }
    }

    pub fn hold(&self) -> Held {
        Held {
            budget: self.clone(),
            permit: None,
        }
    }
}

/// What one request has paid. It goes back to the budget on drop.
pub struct Held {
    budget: Budget,
    permit: Option<OwnedSemaphorePermit>,
}

impl Held {
    fn kib(&self) -> usize {
        self.permit
            .as_ref()
            .map_or(0, OwnedSemaphorePermit::num_permits)
    }

    pub fn take(&mut self, bytes: usize) -> Result<(), Failure> {
        let kib = bytes.div_ceil(KIB);
        if self.kib() + kib > self.budget.kib {
            return Err(too_large(
                "the body needs more memory than one request may use",
            ));
        }
        let busy = || {
            (
                StatusCode::TOO_MANY_REQUESTS,
                "too many bodies in flight".to_owned(),
            )
        };
        let more = u32::try_from(kib)
            .ok()
            .and_then(|n| {
                Arc::clone(&self.budget.permits)
                    .try_acquire_many_owned(n)
                    .ok()
            })
            .ok_or_else(busy)?;
        match &mut self.permit {
            Some(permit) => permit.merge(more),
            None => self.permit = Some(more),
        }
        Ok(())
    }

    /// Gives back what is paid beyond `bytes`, for when no more than that is
    /// still in use.
    pub fn keep(&mut self, bytes: usize) {
        let extra = self.kib().saturating_sub(bytes.div_ceil(KIB));
        self.release_kib(extra);
    }

    /// Gives back `bytes` that are no longer in use.
    pub fn release(&mut self, bytes: usize) {
        self.release_kib(bytes / KIB);
    }

    fn release_kib(&mut self, kib: usize) {
        if let Some(permit) = &mut self.permit {
            drop(permit.split(kib.min(permit.num_permits())));
        }
    }
}

fn too_large(why: &str) -> Failure {
    (StatusCode::PAYLOAD_TOO_LARGE, why.to_owned())
}

/// Makes room for `need` bytes in `buf`, at least doubling it, and pays for
/// the new room. While a buffer grows it holds its old room and its new one,
/// so everything a buffer was ever given stays paid.
fn grow(buf: &mut Vec<u8>, need: usize, limit: usize, held: &mut Held) -> Result<(), Failure> {
    if need <= buf.capacity() {
        return Ok(());
    }
    let room = need.max(2 * buf.capacity()).max(FIRST_ROOM).min(limit);
    held.take(room)?;
    buf.reserve_exact(room - buf.len());
    Ok(())
}

/// A body read frame by frame, which must arrive whole within [`TIMEOUT`].
struct Incoming {
    body: Body,
    deadline: Instant,
    limit: usize,
    bytes: Vec<u8>,
    ended: bool,
}

impl Incoming {
    fn new(body: Body, limit: usize) -> Self {
        Self {
            body,
            deadline: Instant::now() + TIMEOUT,
            limit,
            bytes: Vec::new(),
            ended: false,
        }
    }

    /// Reads one more frame of data. False once the body has ended.
    async fn more(&mut self, held: &mut Held) -> Result<bool, Failure> {
        while !self.ended {
            let frame = tokio::time::timeout_at(self.deadline, self.body.frame())
                .await
                .map_err(|e| (StatusCode::REQUEST_TIMEOUT, format!("body not read: {e}")))?;
            let Some(frame) = frame else {
                self.ended = true;
                break;
            };
            let Ok(data) = frame.map_err(bad)?.into_data() else {
                continue;
            };
            let need = self.bytes.len() + data.len();
            if need > self.limit {
                return Err(too_large("body too large"));
            }
            grow(&mut self.bytes, need, self.limit, held)?;
            self.bytes.extend_from_slice(&data);
            return Ok(true);
        }
        Ok(false)
    }
}

pub async fn read(body: Body, held: &mut Held) -> Result<Vec<u8>, Failure> {
    read_at_most(body, COMPRESSED, held).await
}

/// Reads a body of at most `limit` bytes as sent, `413` beyond.
pub async fn read_at_most(body: Body, limit: usize, held: &mut Held) -> Result<Vec<u8>, Failure> {
    let mut incoming = Incoming::new(body, limit);
    while incoming.more(held).await? {}
    Ok(incoming.bytes)
}

/// Reads a body of at most `limit` bytes that must arrive whole `within`,
/// without paying from a budget. For a body smaller than what its connection
/// already holds: a budget would bound nothing, and slow bodies that spend it
/// would refuse everyone else.
pub async fn read_small(
    mut body: Body,
    limit: usize,
    within: Duration,
) -> Result<Vec<u8>, Failure> {
    let deadline = Instant::now() + within;
    let mut bytes = Vec::new();
    while let Some(frame) = tokio::time::timeout_at(deadline, body.frame())
        .await
        .map_err(|e| (StatusCode::REQUEST_TIMEOUT, format!("body not read: {e}")))?
    {
        let Ok(data) = frame.map_err(bad)?.into_data() else {
            continue;
        };
        if bytes.len() + data.len() > limit {
            return Err(too_large("body too large"));
        }
        bytes.extend_from_slice(&data);
    }
    Ok(bytes)
}

/// Reads an envelope whose key is in the DSN of its header line. Reads only
/// until that line decompresses, passes it to `admit`, and reads the rest only
/// if `admit` succeeds, so a refused request holds about [`BEFORE_AUTH`].
pub async fn read_envelope(
    body: Body,
    held: &mut Held,
    encoding: &str,
    admit: impl FnOnce(&EnvelopeHeader) -> Result<(), Failure>,
) -> Result<Vec<u8>, Failure> {
    let mut incoming = Incoming::new(body, COMPRESSED);
    // Each try decompresses from the start, so tries wait for the body to
    // double, and a body sent a byte at a time costs a few tries, not one
    // per byte.
    let mut tried = 0;
    loop {
        let more = incoming.more(held).await?;
        let len = incoming.bytes.len();
        if more && len < 2 * tried && len < BEFORE_AUTH {
            continue;
        }
        tried = len;
        // The line, and the header's two fields that come from it.
        let paid = decoder_memory(&incoming.bytes, encoding) + 2 * MAX_HEADER_LINE;
        held.take(paid)?;
        let header = header_line(&incoming.bytes, encoding, !more)?
            .map(|line| EnvelopeHeader::parse(&line).map_err(bad))
            .transpose();
        held.release(paid);
        if let Some(header) = header? {
            admit(&header)?;
            break;
        }
        if len >= BEFORE_AUTH {
            return Err(bad("no envelope header line in the first 128 KiB"));
        }
    }
    while incoming.more(held).await? {}
    Ok(incoming.bytes)
}

/// The first line of the body in `bytes`, once it is there. When `whole`,
/// `bytes` is the whole body, and a body of one line without a newline is
/// that line.
fn header_line(bytes: &[u8], encoding: &str, whole: bool) -> Result<Option<Vec<u8>>, Failure> {
    let mut line = Vec::new();
    let read = decoder(bytes, encoding)?
        .take(MAX_HEADER_LINE as u64)
        .read_until(b'\n', &mut line);
    if line.last() == Some(&b'\n') {
        return Ok(Some(line));
    }
    if line.len() >= MAX_HEADER_LINE {
        return Err(bad("envelope header line too long"));
    }
    if whole {
        read.map_err(bad)?;
        return Ok(Some(line));
    }
    Ok(None)
}

/// What a decoder for `encoding` allocates at most while it reads the body
/// `bytes`.
pub fn decoder_memory(bytes: &[u8], encoding: &str) -> usize {
    match encoding {
        // A gzip header can add an extra field, a file name and a comment
        // of up to 64 KiB each, and the last two grow by doubling.
        "gzip" => 128 * KIB + 256 * KIB,
        "deflate" => 128 * KIB,
        "zstd" => (1 << ZSTD_WINDOW_LOG) + MIB,
        "br" => brotli_window(bytes) + BROTLI_TABLES,
        _ => 0,
    }
}

/// The Huffman tables of a brotli stream. At most 256 trees in each of its
/// three groups take about 2.6 MiB, which the decoder may hold one and a
/// half times over, and it holds one group's old tables while they grow.
const BROTLI_TABLES: usize = 6 * MIB;

/// The window a brotli stream asks for in its first bits (RFC 7932, 9.1).
fn brotli_window(bytes: &[u8]) -> usize {
    let first = bytes.first().copied().unwrap_or(0);
    let log = match (first & 1, (first >> 1) & 7, (first >> 4) & 7) {
        (0, _, _) => 16,
        (_, n @ 1.., _) => 17 + n,
        (_, 0, n @ 1..) => 8 + n,
        _ => 17,
    };
    1 << log
}

/// A reader of the body `bytes` decompressed. It allocates no more than
/// [`decoder_memory`].
pub fn decoder<'a>(bytes: &'a [u8], encoding: &str) -> Result<impl BufRead + 'a, Failure> {
    let source = Cursor::new(bytes);
    let reader: Box<dyn Read + 'a> = match encoding {
        "" | "identity" => Box::new(source),
        "gzip" => Box::new(flate2::read::GzDecoder::new(source)),
        "deflate" => Box::new(flate2::read::ZlibDecoder::new(source)),
        "br" => {
            let mut d = brotli_decompressor::Decompressor::new(source, 4096);
            // Large-window brotli is an extension that HTTP's brotli (RFC
            // 7932) does not have, and its window can be 1 GiB.
            d.set_parameter(BrotliDecoderParameter::BROTLI_DECODER_PARAM_LARGE_WINDOW, 0);
            // The whole window at once. Grown by doubling, it would hold one
            // and a half windows while it grows.
            d.set_initial_ring_buffer_size(u32::MAX);
            Box::new(d)
        }
        "zstd" => {
            let mut d = zstd::stream::read::Decoder::new(source).map_err(bad)?;
            d.window_log_max(ZSTD_WINDOW_LOG).map_err(bad)?;
            Box::new(d)
        }
        _ => return Err(bad("unsupported content encoding")),
    };
    Ok(BufReader::new(reader))
}

/// Decompresses no more than `limit` bytes, for a caller that checks whether
/// there were more, and is trusted not to send many bodies at once.
pub fn inflate(bytes: &[u8], encoding: &str, limit: usize) -> Result<Vec<u8>, Failure> {
    let mut out = Vec::new();
    decoder(bytes, encoding)?
        .take(limit as u64)
        .read_to_end(&mut out)
        .map_err(bad)?;
    Ok(out)
}

/// Decompresses a body read by [`read`], paying for each step its buffer
/// grows, and gives back what the compressed bytes held.
pub fn decode(bytes: Vec<u8>, encoding: &str, held: &mut Held) -> Result<Vec<u8>, Failure> {
    let too_large = || too_large("decompressed body too large");
    if matches!(encoding, "" | "identity") {
        if bytes.len() > DECOMPRESSED {
            return Err(too_large());
        }
        held.keep(bytes.capacity());
        return Ok(bytes);
    }
    // Paid back below, when only `out` is still in use.
    held.take(decoder_memory(&bytes, encoding))?;
    let mut out = Vec::new();
    {
        let mut reader = decoder(&bytes, encoding)?;
        loop {
            // Reading no further than the room left keeps the buffer from
            // growing unpaid.
            let room = out.capacity() - out.len();
            (&mut reader)
                .take(room as u64)
                .read_to_end(&mut out)
                .map_err(bad)?;
            if out.len() < out.capacity() {
                break;
            }
            let mut next = [0];
            if reader.read(&mut next).map_err(bad)? == 0 {
                break;
            }
            if out.len() >= DECOMPRESSED {
                return Err(too_large());
            }
            let need = out.len() + 1;
            grow(&mut out, need, DECOMPRESSED, held)?;
            out.push(next[0]);
        }
    }
    drop(bytes);
    held.keep(out.capacity());
    Ok(out)
}

#[cfg(test)]
#[path = "body_tests.rs"]
mod tests;
