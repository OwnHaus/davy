#![no_main]

use davy::logs::logtail::{cost, parse};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The server refuses what it cannot price, so a price must exist for
    // every body that parses.
    if parse(data, "application/msgpack", 1, 0).is_ok() {
        assert!(cost(data, "application/msgpack").is_ok());
    }
});
