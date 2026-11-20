#![no_main]

use davy::logs::sentry::{cost, parse};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The server drops an item it cannot price, so a price must exist for
    // every item that parses.
    if parse(data, 1, 0).is_ok() {
        assert!(cost(data).is_ok());
    }
});
