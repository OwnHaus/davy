#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = davy::logs::logtail::parse(data, "application/msgpack", 1, 0);
});
