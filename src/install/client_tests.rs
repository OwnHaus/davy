use super::*;

fn url() -> Url {
    Url::parse("https://davy.example.com/").unwrap()
}

const SOURCE: &str = "/var/lib/davy/token";

#[test]
fn claude_code_reads_the_token_from_the_environment() {
    assert_eq!(
        config(Client::ClaudeCode, &url(), &Token::Env, SOURCE).unwrap(),
        r#"Merge this into .mcp.json in the project root, next to the servers already there.
Set DAVY_TOKEN, in the environment the client starts from, to the contents of /var/lib/davy/token.

{
  "mcpServers": {
    "davy": {
      "type": "http",
      "url": "https://davy.example.com/mcp",
      "headers": {
        "Authorization": "Bearer ${DAVY_TOKEN}"
      }
    }
  }
}
"#
    );
}

#[test]
fn cursor_uses_its_own_env_syntax() {
    assert_eq!(
        config(Client::Cursor, &url(), &Token::Env, SOURCE).unwrap(),
        r#"Merge this into .cursor/mcp.json in the project root, next to the servers already there.
Set DAVY_TOKEN, in the environment the client starts from, to the contents of /var/lib/davy/token.

{
  "mcpServers": {
    "davy": {
      "url": "https://davy.example.com/mcp",
      "headers": {
        "Authorization": "Bearer ${env:DAVY_TOKEN}"
      }
    }
  }
}
"#
    );
}

#[test]
fn codex_names_the_variable_that_holds_the_bearer_token() {
    assert_eq!(
        config(Client::Codex, &url(), &Token::Env, SOURCE).unwrap(),
        r#"Append this to ~/.codex/config.toml.
Set DAVY_TOKEN, in the environment the client starts from, to the contents of /var/lib/davy/token.

[mcp_servers.davy]
url = "https://davy.example.com/mcp"
bearer_token_env_var = "DAVY_TOKEN"
"#
    );
}

#[test]
fn a_literal_token_is_escaped_and_comes_with_a_warning() {
    let token = Token::Literal(r#"ab"c\d"#.to_owned());
    assert_eq!(
        config(Client::Codex, &url(), &token, SOURCE).unwrap(),
        r#"Append this to ~/.codex/config.toml.
This config holds the admin token. Keep the file out of version control.

[mcp_servers.davy]
url = "https://davy.example.com/mcp"
http_headers = { "Authorization" = "Bearer ab\"c\\d" }
"#
    );
    let json = config(Client::ClaudeCode, &url(), &token, SOURCE).unwrap();
    assert!(
        json.contains(r#""Authorization": "Bearer ab\"c\\d""#),
        "{json}"
    );
}

#[test]
fn the_mcp_path_is_joined_to_a_public_url_with_a_path() {
    let url = Url::parse("https://example.com/davy/").unwrap();
    let toml = config(Client::Codex, &url, &Token::Env, SOURCE).unwrap();
    assert!(
        toml.contains("url = \"https://example.com/davy/mcp\"\n"),
        "{toml}"
    );
}

#[test]
fn run_reads_the_public_url_and_token_from_the_data_directory() {
    let data = crate::test_support::scratch("client");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        data.join("setup.json"),
        r#"{"public_url":"https://davy.example.com/","listen":"127.0.0.1:8000","mail_from":null}"#,
    )
    .unwrap();
    std::fs::write(data.join("token"), "0123456789abcdef0123456789abcdef\n").unwrap();
    let args = |show_token| Args {
        client: Client::Codex,
        data: Some(data.clone()),
        url: None,
        show_token,
    };
    let mut out = Vec::new();
    run(&mut out, &args(true)).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.ends_with(
            "http_headers = { \"Authorization\" = \"Bearer 0123456789abcdef0123456789abcdef\" }\n"
        ),
        "{text}"
    );
    let mut out = Vec::new();
    run(&mut out, &args(false)).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains(&format!(
            "to the contents of {}.\n",
            data.join("token").display()
        )),
        "{text}"
    );
}

#[test]
fn run_refuses_a_url_that_is_not_http() {
    let args = Args {
        client: Client::Cursor,
        data: None,
        url: Some(Url::parse("ftp://davy.example.com/").unwrap()),
        show_token: false,
    };
    assert_eq!(
        run(&mut Vec::new(), &args).unwrap_err().to_string(),
        "the public URL must start with https:// or http:// and name a host, e.g. https://davy.example.com"
    );
}

#[test]
fn the_token_goes_out_over_https_or_to_this_machine_only() {
    let config = |url: &str| {
        config(
            Client::Codex,
            &Url::parse(url).unwrap(),
            &Token::Env,
            "token",
        )
    };
    assert!(config("http://127.0.0.1:8000/").is_ok());
    assert!(config("http://localhost:8000/").is_err());
    assert_eq!(
        config("http://davy.example.com/").unwrap_err().to_string(),
        "http://davy.example.com/ is plain http, so the agent would send the admin token unencrypted, put Davy behind https first (docs/install.md, step 7)"
    );
}
