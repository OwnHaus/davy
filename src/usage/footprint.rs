//! What the footprint benchmark measured, as constants the forecast uses
//! until a server has measured itself (`docs/storage-benchmark.md`,
//! "Footprint for the forecast"). A test pins each one to
//! `bench/footprint.json`, so a rerun cannot leave the binary behind.

/// Low, middle and high value of a ratio, in parts per million.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Band {
    pub low: u64,
    pub mid: u64,
    pub high: u64,
}

/// How logs pack on disk where the server has not measured it yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prior {
    /// Disk bytes per raw byte of a log line.
    pub ratio: Band,
    /// What a day index adds to the disk beyond its lines.
    pub day_index: u64,
    /// Where the numbers come from, for the answer to name.
    pub source: &'static str,
}

/// `coefficients.logs.disk_per_raw_real` (min, median, max) times 10^6
/// and `coefficients.logs.extra_bytes_per_day_index.median`.
pub const PRIOR: Prior = Prior {
    ratio: Band {
        low: 546_000,
        mid: 683_600,
        high: 1_026_800,
    },
    day_index: 281_220,
    source: "bench/footprint.json, measured 2026-11-25 at v0.1.0-172-g469fd53 on Apple M2 Max (Mac14,5)",
};

/// The prior every forecast test passes, so their numbers stay as they
/// are when a rerun changes [`PRIOR`].
#[cfg(test)]
pub const FIXTURE: Prior = Prior {
    ratio: Band {
        low: 563_600,
        mid: 761_000,
        high: 1_004_100,
    },
    day_index: 632_556,
    source: "fixture",
};

#[cfg(test)]
#[path = "footprint_tests.rs"]
mod tests;
