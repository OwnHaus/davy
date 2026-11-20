#![no_main]

use davy::logs::logtail::{cost, parse};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for content_type in ["application/json", "application/x-ndjson"] {
        // The server refuses what it cannot price, so a price must exist
        // for every body that parses.
        if parse(data, content_type, 1, 0).is_ok() {
            assert!(cost(data, content_type).is_ok());
        }
    }
});
