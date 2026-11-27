use super::{ColdNote, ColdScan};

#[test]
fn a_note_gives_times_in_rfc_3339_and_failures_only_when_some() {
    let scan = ColdScan {
        from_ns: 1_795_000_000_000_000_000,
        to_ns: 1_795_086_399_999_999_999,
        took_ms: 120,
        rows: 40_000,
        unscanned_to_ns: None,
        corrupt_files: 0,
        corrupt_blocks: 0,
        corrupt_rows: 0,
    };
    let note = ColdNote::from(&scan);
    assert_eq!(
        serde_json::to_string(&note).unwrap(),
        r#"{"cold_from":"2026-11-18T11:06:40Z","cold_to":"2026-11-19T11:06:39.999999999Z","scan_ms":120,"rows_read":40000}"#
    );
    assert_eq!(
        note.line(),
        "cold logs from 2026-11-18T11:06:40Z to 2026-11-19T11:06:39.999999999Z: 40000 rows read in 120 ms"
    );

    let short = ColdNote::from(&ColdScan {
        unscanned_to_ns: Some(1_795_040_000_000_000_000),
        corrupt_blocks: 1,
        corrupt_rows: 900,
        ..scan
    });
    assert_eq!(
        serde_json::to_value(&short).unwrap()["unscanned_to"],
        "2026-11-18T22:13:20Z"
    );
    assert_eq!(serde_json::to_value(&short).unwrap()["corrupt_rows"], 900);
    assert!(short.line().ends_with(
        "; the time budget ran out, so logs up to 2026-11-18T22:13:20Z may be missing; 900 rows of 1 blocks and 0 files left out, as they failed a check (davy doctor names them)"
    ));
}
