use super::{OTHER, of, parse, show, template};

fn assert_templates(cases: &[(&str, &str)]) {
    for (line, expected) in cases {
        assert_eq!(template(line), *expected, "line: {line}");
    }
}

#[test]
fn nginx_access_lines_keep_the_route_and_mask_the_values() {
    assert_templates(&[
        (
            r#"203.0.113.7 - - [24/Nov/2026:10:23:45 +0000] "GET /api/users/123?page=2 HTTP/1.1" 200 1534 "https://example.com/" "Mozilla/5.0 (X11; Linux x86_64)""#,
            r#"<IP> - - [<NUM>/Nov/<NUM> +<NUM>] "GET /api/users/<NUM>?page=<NUM> HTTP/<NUM>" <NUM> <NUM> <STR> <STR>"#,
        ),
        (
            r#"[Filtered: ip] - - [24/Nov/2026:10:23:46 +0000] "POST /login HTTP/2.0" 302 0 "-" "curl/8.4.0""#,
            r#"<IP> - - [<NUM>/Nov/<NUM> +<NUM>] "POST /login HTTP/<NUM>" <NUM> <NUM> <STR> <STR>"#,
        ),
    ]);
}

#[test]
fn json_app_lines_keep_keys_and_prose_and_mask_values() {
    assert_templates(&[
        (
            r#"{"level":"error","msg":"payment failed for order 8812","order_id":8812,"duration_ms":31.5,"trace":"4bf92f3577b34da6a3ce929d0e0e4736"}"#,
            r#"{"level":<STR>, "msg":"payment failed for order <NUM>", "order_id":<NUM>, "duration_ms":<NUM>, "trace":<STR>}"#,
        ),
        (
            "request completed in 23ms status=200 user_id=8812 request_id=550e8400-e29b-41d4-a716-446655440000",
            "request completed in <NUM>ms status=<NUM> user_id=<NUM> request_id=<UUID>",
        ),
    ]);
}

#[test]
fn syslog_lines_mask_time_host_numbers_and_pids() {
    assert_templates(&[
        (
            "Nov 24 10:23:45 web-01 sshd[4721]: Failed password for invalid user admin from 198.51.100.23 port 52113 ssh2",
            "Nov <NUM> <NUM> web-<NUM> sshd[<NUM>]: Failed password for invalid user admin from",
        ),
        (
            "Nov 24 10:23:46 db-2 kernel: [12345.678901] Out of memory: Killed process 3112 (postgres) total-vm:4194304kB",
            "Nov <NUM> <NUM> db-<NUM> kernel: [<NUM>] Out of memory: Killed process <NUM>",
        ),
    ]);
}

#[test]
fn java_and_spark_lines_mask_timestamps_threads_and_counters() {
    assert_templates(&[
        (
            "24/11/24 10:23:45 INFO Executor: Finished task 3.0 in stage 1.0 (TID 7). 1840 bytes result sent to driver",
            "<NUM> <NUM> INFO Executor: Finished task <NUM> in stage <NUM> (TID <NUM>).",
        ),
        (
            "2026-11-24 10:23:45,120 ERROR [http-nio-8080-exec-3] c.e.shop.OrderService - Order 8812 failed: java.net.SocketTimeoutException: Read timed out",
            "<NUM> <NUM> ERROR [http-nio-<NUM>-exec-<NUM>] c.e.shop.OrderService - Order <NUM> failed: java.net.SocketTimeoutException: Read timed",
        ),
    ]);
}

#[test]
fn hdfs_lines_mask_blocks_addresses_and_sizes() {
    assert_templates(&[
        (
            "081109 203615 148 INFO dfs.DataNode$PacketResponder: PacketResponder 1 for block blk_38865049064139660 terminating",
            "<NUM> <NUM> <NUM> INFO dfs.DataNode$PacketResponder: PacketResponder <NUM> for block blk_<NUM> terminating",
        ),
        (
            "081109 204005 35 INFO dfs.FSNamesystem: BLOCK* NameSystem.addStoredBlock: blockMap updated: 10.251.73.220:50010 is added to blk_7128370237687728475 size 67108864",
            "<NUM> <NUM> <NUM> INFO dfs.FSNamesystem: BLOCK* NameSystem.addStoredBlock: blockMap updated: <IP> is added",
        ),
    ]);
}

#[test]
fn lines_that_differ_only_in_values_share_an_id() {
    let a = of(
        "081109 203807 222 INFO dfs.DataNode$PacketResponder: Received block blk_-1608999687919862906 of size 91178 from /10.250.10.6",
    );
    let b = of(
        "081110 101010 7 INFO dfs.DataNode$PacketResponder: Received block blk_42 of size 1 from /10.0.0.1",
    );
    assert_eq!(a, b);
    assert_ne!(a.id, of("connection reset by peer").id);
}

#[test]
fn hex_uuids_emails_and_ipv6_are_masked() {
    assert_templates(&[
        ("cache miss key=0x7f3a2b1c", "cache miss key=<HEX>"),
        (
            "commit 9fceb02d0ae598e95dc970b74767f19372d61af8 deployed",
            "commit <HEX> deployed",
        ),
        ("cache deadbeef", "cache deadbeef"),
        (
            "job 550e8400-e29b-41d4-a716-446655440000 done",
            "job <UUID> done",
        ),
        (
            "invite sent to jane.doe@example.com",
            "invite sent to <EMAIL>",
        ),
        ("connect to 2001:db8::1 failed", "connect to <IP> failed"),
        ("started at 2026-11-24T10:23:45.003Z", "started at <TS>"),
    ]);
}

#[test]
fn scrub_markers_become_placeholders_or_stay_one_token() {
    assert_templates(&[
        (
            "login failed for [Filtered: email] from [Filtered: ip]",
            "login failed for <EMAIL> from <IP>",
        ),
        (
            "auth password=[Filtered: sensitive] token [Filtered: credential] rejected",
            "auth password=[Filtered: sensitive] token [Filtered: credential] rejected",
        ),
    ]);
}

#[test]
fn quoted_values_are_masked_but_apostrophes_and_open_quotes_are_text() {
    assert_templates(&[
        (
            r#"search for "red shoes" returned 0 results"#,
            "search for <STR> returned <NUM> results",
        ),
        (
            "deleted 'old report' by request",
            "deleted <STR> by request",
        ),
        ("user's cart can't be saved", "user's cart can't be saved"),
        (r#"say "hi to all"#, r#"say "hi to all"#),
    ]);
}

#[test]
fn only_the_first_twelve_tokens_count() {
    assert_eq!(
        template("a b c d e f g h i j k l m n"),
        "a b c d e f g h i j k l"
    );
    assert_eq!(
        of("a b c d e f g h i j k l m").id,
        of("a  b c d e f g h i j k l other tail").id
    );
}

#[test]
fn a_long_token_is_cut_at_64_bytes() {
    let long = "x".repeat(100);
    assert_eq!(template(&long), format!("{}...", "x".repeat(64)));
    let wide = "é".repeat(40);
    assert_eq!(template(&wide), format!("{}...", "é".repeat(32)));
}

#[test]
fn only_the_first_4_kib_of_a_message_is_read() {
    let line = format!("{} tail", "a".repeat(5000));
    assert_eq!(template(&line), format!("{}...", "a".repeat(64)));
    let spaced = format!("{}tail", " ".repeat(4096));
    assert_eq!(template(&spaced), "");
}

#[test]
fn ids_are_stable_48_bit_and_never_other() {
    let id = of("connection reset by peer").id;
    assert_eq!(show(id), "bdbaa8ee5bae");
    assert!(id < 1 << 48);
    assert_ne!(of("").id, OTHER);
}

#[test]
fn ids_print_as_12_hex_digits_and_parse_back() {
    assert_eq!(show(0xab), "0000000000ab");
    assert_eq!(show(OTHER), "other");
    assert_eq!(parse("0000000000ab"), Ok(0xab));
    assert_eq!(parse(" other "), Ok(OTHER));
    for bad in ["ab", "0000000000ag", "00000000000ab", "+0000000000a", ""] {
        assert!(parse(bad).is_err(), "{bad}");
    }
}
