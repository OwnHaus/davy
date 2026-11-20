#![no_main]

use davy::{envelope::Envelope, event::ErrorEvent, grouping};
use libfuzzer_sys::fuzz_target;

// The envelope and the event items in it, as the ingest endpoint reads them.
fuzz_target!(|data: &[u8]| {
    let Ok(envelope) = Envelope::parse(data) else {
        return;
    };
    for item in envelope.items {
        if let Ok(value) = serde_json::from_slice(item.payload) {
            if let Ok(event) = ErrorEvent::parse(value) {
                grouping::group(&event);
            }
        }
    }
});
