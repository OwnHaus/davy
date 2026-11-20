#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = davy::logs::logtail::parse(data, "application/json", 1, 0);
    let _ = davy::logs::logtail::parse(data, "application/x-ndjson", 1, 0);
});
