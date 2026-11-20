#![no_main]

use davy::logs::otlp::{Encoding, parse};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse(data, Encoding::Json, 1, 0);
});
