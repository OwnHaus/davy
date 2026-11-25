use super::*;

const SECRET: &str = "kZ4Xc9vQ2mLp8RtY6wNb3HdF";

fn args(data: &std::path::Path, secret_stdin: bool, no_secret: bool) -> Args {
    Args {
        slug: "shop".to_owned(),
        data: data.to_owned(),
        secret_stdin,
        no_secret,
    }
}

fn printed(out: &[u8]) -> serde_json::Value {
    serde_json::from_slice(out).unwrap()
}

#[test]
fn the_cli_sets_reports_and_clears_the_secret_without_printing_it() {
    let data = crate::test_support::scratch("vercel-cli");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "shop").unwrap();

    let mut out = Vec::new();
    run(&mut out, &args(&data, false, false), &b""[..]).unwrap();
    assert_eq!(
        printed(&out),
        json!({"project": "shop", "signature_required": false})
    );

    let mut out = Vec::new();
    let stdin = format!("{SECRET}\n");
    run(&mut out, &args(&data, true, false), stdin.as_bytes()).unwrap();
    assert_eq!(
        printed(&out),
        json!({"project": "shop", "signature_required": true})
    );
    assert!(!String::from_utf8(out).unwrap().contains(SECRET));
    assert_eq!(secret(&c, 1).unwrap().as_deref(), Some(SECRET));

    let mut out = Vec::new();
    run(&mut out, &args(&data, false, true), &b""[..]).unwrap();
    assert_eq!(
        printed(&out),
        json!({"project": "shop", "signature_required": false})
    );
    assert_eq!(secret(&c, 1).unwrap(), None);

    let unknown = Args {
        slug: "nope".to_owned(),
        ..args(&data, false, false)
    };
    assert!(run(&mut Vec::new(), &unknown, &b""[..]).is_err());
    drop(c);
    std::fs::remove_dir_all(data).unwrap();
}

#[test]
fn a_secret_is_one_line_of_text_without_the_space_around_it() {
    for input in [
        format!("{SECRET}\r\n"),
        SECRET.to_owned(),
        format!("{SECRET} \n"),
        format!("  {SECRET}\t\n"),
        format!("\n{SECRET}\n\n"),
    ] {
        assert_eq!(read_secret(input.as_bytes()).unwrap(), SECRET, "{input:?}");
    }
    let long = "x".repeat(MAX_SECRET);
    assert_eq!(read_secret(long.as_bytes()).unwrap(), long);
    let padded = format!("{}{long}\n", " ".repeat(MAX_SECRET - 1));
    assert_eq!(read_secret(padded.as_bytes()).unwrap(), long);
    let refused = |input: &[u8]| read_secret(input).unwrap_err().to_string();
    let empty = "no secret on standard input, pipe in the drain's signature verification secret";
    for input in ["", "\n", "  \r\n", " \t "] {
        assert_eq!(refused(input.as_bytes()), empty, "{input:?}");
    }
    let too_long = "the secret is longer than 1024 bytes";
    assert_eq!(refused("x".repeat(MAX_SECRET + 1).as_bytes()), too_long);
    let past_room = format!("{}{long}", " ".repeat(MAX_SECRET + 1));
    assert_eq!(refused(past_room.as_bytes()), too_long);
    let mut cut = "\u{e9}".repeat(MAX_SECRET);
    cut.insert(0, 'x');
    assert_eq!(refused(cut.as_bytes()), too_long);
    let one_line = "the secret is one line of text, without control characters";
    for input in [format!("{SECRET}\n{SECRET}\n"), "a\tb".to_owned()] {
        assert_eq!(refused(input.as_bytes()), one_line, "{input:?}");
    }
    assert_eq!(
        refused(&[0xff_u8, 0xfe][..]),
        "the secret on standard input is not UTF-8 text"
    );
}

#[test]
fn a_store_without_the_column_gains_it() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE projects(id INTEGER PRIMARY KEY);")
        .unwrap();
    c.execute_batch(MIGRATION).unwrap();
    c.execute("INSERT INTO projects(id) VALUES (1)", [])
        .unwrap();
    assert_eq!(secret(&c, 1).unwrap(), None);
    assert_eq!(secret(&c, 2).unwrap(), None);
    c.execute_batch(UNDO).unwrap();
    assert!(secret(&c, 1).is_err());
}
