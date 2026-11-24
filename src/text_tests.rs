use super::{printable, size};

#[test]
fn control_characters_and_marks_that_reorder_text_are_replaced() {
    let s = "a\u{202E}b\u{1b}c\u{9b}d\u{2066}e\u{200F}f\u{061C}g";
    assert_eq!(printable(s, '?'), "a?b?c?d?e?f?g");
    assert_eq!(printable("plain é", '?'), "plain é");
}

#[test]
fn sizes_read_in_three_digits_and_powers_of_1000() {
    let cases = [
        (0, "0 B"),
        (999, "999 B"),
        (1000, "1.0 KB"),
        (1234, "1.2 KB"),
        (99_949, "99.9 KB"),
        (99_950, "100 KB"),
        (999_499, "999 KB"),
        (999_500, "1.0 MB"),
        (999_950, "1.0 MB"),
        (56_000_000, "56.0 MB"),
        (1_500_000_000_000, "1.5 TB"),
        (u64::MAX, "18447 PB"),
    ];
    for (bytes, shown) in cases {
        assert_eq!(size(bytes), shown, "{bytes}");
    }
}
