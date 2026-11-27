#![no_main]

use davy::logs::store::cold::bench::read;
use libfuzzer_sys::fuzz_target;

// Bytes read as a cold file: the footer, then every block checked,
// decompressed and matched. Each block may take at most 16 MiB
// decompressed, so a bomb shows up as an error and not as an out of
// memory.
fuzz_target!(|data: &[u8]| {
    let _ = read(data, None);
    let _ = read(data, Some("attributes.status:500 OR \"timeout\""));
});
