#![no_main]

use davy::sourcemaps::bundle::{Limit, unpack};
use libfuzzer_sys::fuzz_target;

// A small limit, so a zip bomb shows up as an error and not as an out of memory.
const LIMIT: Limit = Limit {
    files: 64,
    bytes: 1 << 20,
};

fuzz_target!(|data: &[u8]| {
    let _ = unpack(data, LIMIT);
});
