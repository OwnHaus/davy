#![no_main]

use davy::{event::ErrorEvent, grouping};
use libfuzzer_sys::fuzz_target;

// An event and the grouping that runs on its strings.
fuzz_target!(|data: &[u8]| {
    if let Ok(mut value) = serde_json::from_slice(data) {
        davy::scrub::Policy::default().value(&mut value);
        if let Ok(event) = ErrorEvent::parse(value) {
            grouping::group(&event);
        }
    }
});
