#![no_main]

use davy::logs::otlp::{Encoding, cost, decode, parse};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The server refuses what it cannot price, so a price must exist for
    // every request that decodes.
    if decode(data, Encoding::Protobuf).is_ok() {
        assert!(cost(data, Encoding::Protobuf).is_ok());
    }
    let _ = parse(data, Encoding::Protobuf, 1, 0);
});
