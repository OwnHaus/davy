#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = davy::logs::sentry::parse(data, 1, 0);
});
