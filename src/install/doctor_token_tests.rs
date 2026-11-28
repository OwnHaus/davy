//! The doctor check of DATA/token.

use super::super::{Status, not_ok};
use super::file;
use std::path::Path;

#[test]
fn an_agent_tokens_secret_in_the_token_file_fails() {
    let secret = format!("{}{}", crate::tokens::PREFIX, "a".repeat(43));
    let path = Path::new("/srv/davy/token");
    assert_eq!(
        file(path, Some(&format!("{secret}\n")), "/srv/davy"),
        not_ok(
            "token",
            Status::Fail,
            "/srv/davy/token holds an agent token's secret, as it starts with davy_agent_, and davy serve refuses it as its admin token".to_owned(),
            "move the secret to the agent's MCP client, then rm /srv/davy/token && davy setup --data /srv/davy --public-url URL".to_owned(),
        )
    );
    // A token that only holds the prefix further on is no secret.
    let admin = format!("x{secret}");
    assert_eq!(file(path, Some(&admin), "/srv/davy").status, Status::Ok);
}
