//! Who may call which MCP tool. [`need`] is the one table of what each tool
//! needs, mirrored in "Agent tokens" in docs/design.md; the MCP server asks
//! it when it lists the tools and before every call, and nothing else
//! decides. See src/tokens.rs for the tokens themselves.

use serde::{Deserialize, Serialize};

/// How far an agent token reaches, lowest first. Each level holds every
/// right of the ones below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// The tools that only read.
    Read,
    /// Changes a person undoes in a click: resolve an issue, stage or
    /// remove a shadow drop rule, send feedback.
    Triage,
    /// Configuration that loses nothing: keep data longer.
    Full,
}

impl Level {
    pub const ALL: [Self; 3] = [Self::Read, Self::Triage, Self::Full];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Triage => "triage",
            Self::Full => "full",
        }
    }

    /// A level by its name, as the CLI and the database write it.
    pub fn parse(s: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|l| l.as_str() == s)
            .ok_or_else(|| format!("unknown level {s:?}: give read, triage or full"))
    }
}

/// What one call needs: a level, and for a change that deletes data or
/// stops keeping it, the destructive unlock on top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Need {
    pub level: Level,
    pub destructive: bool,
}

impl Need {
    const fn at(level: Level) -> Self {
        Self {
            level,
            destructive: false,
        }
    }

    /// Words for a refusal: `triage`, or `full with destructive changes
    /// unlocked`.
    pub fn words(self) -> String {
        if self.destructive {
            format!("{} with destructive changes unlocked", self.level.as_str())
        } else {
            self.level.as_str().to_owned()
        }
    }
}

/// A destructive change needs a full token that a person unlocked. Only
/// a full token can be unlocked, see src/tokens.rs.
pub const DESTRUCTIVE: Need = Need {
    level: Level::Full,
    destructive: true,
};

/// The need of every MCP tool, the classification table of docs/design.md.
/// `None` for a name no tool has. `set_retention` needs [`DESTRUCTIVE`]
/// when its change would delete data, which only the stored settings tell,
/// see `mcp::access`.
pub fn need(tool: &str) -> Option<Need> {
    Some(match tool {
        "schema" | "sql" | "search_logs" | "count_logs" | "log_patterns" | "connect_project"
        | "project_status" | "usage" | "drop_rules" | "release_health" | "issue_context" => {
            Need::at(Level::Read)
        }
        "mark_fixed" | "submit_feedback" | "drop_rule_shadow" | "drop_rule_remove" => {
            Need::at(Level::Triage)
        }
        "set_retention" => Need::at(Level::Full),
        "drop_rule_live" => DESTRUCTIVE,
        _ => return None,
    })
}

/// What a caller holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    pub level: Level,
    /// A person unlocked destructive changes for this token.
    pub destructive: bool,
}

impl Grant {
    /// The admin token reaches every level and, as before agent tokens,
    /// no destructive change: it sits in every agent config made before
    /// them, and an agent's prompt can carry text someone else wrote.
    pub const ADMIN: Self = Self {
        level: Level::Full,
        destructive: false,
    };

    pub fn admits(self, need: Need) -> bool {
        self.level >= need.level && (self.destructive || !need.destructive)
    }
}

/// Proof that an agent token a person unlocked asked for a destructive
/// change. Only [`Caller::unlocked`] makes one, so a writer job without it
/// is refused. It names the token, as the writer reads the unlock again
/// before it applies the change: a person may lock or revoke the token
/// while the call waits for the writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unlocked {
    token: i64,
}

impl Unlocked {
    /// The row of the token in `agent_tokens`.
    pub fn token(self) -> i64 {
        self.token
    }
}

/// Who called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Actor {
    /// The admin token of `DAVY_TOKEN`, `DAVY_TOKEN_FILE` or `DATA/token`.
    Admin,
    /// An agent token, by its row and name.
    Token { id: i64, name: String },
}

impl Actor {
    /// The name a refusal and the ledger give.
    pub fn name(&self) -> &str {
        match self {
            Self::Admin => "admin token",
            Self::Token { name, .. } => name,
        }
    }
}

/// The caller of one `/mcp` request. The token check puts it in the
/// request's extensions, and the tools read it there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub actor: Actor,
    pub grant: Grant,
}

impl Caller {
    pub const fn admin() -> Self {
        Self {
            actor: Actor::Admin,
            grant: Grant::ADMIN,
        }
    }

    /// The proof a destructive job carries, when the caller is an agent
    /// token whose grant reaches one. The admin token never does.
    pub fn unlocked(&self) -> Option<Unlocked> {
        match self.actor {
            Actor::Token { id, .. } if self.grant.admits(DESTRUCTIVE) => {
                Some(Unlocked { token: id })
            }
            _ => None,
        }
    }

    /// The tool error for a call `need` does not admit. It names the level
    /// the call needs and the one the caller holds, and who can raise it.
    pub fn refusal(&self, tool: &str, need: Need) -> String {
        let held = if self.grant.destructive {
            Need {
                level: self.grant.level,
                destructive: true,
            }
            .words()
        } else {
            self.grant.level.as_str().to_owned()
        };
        let who = match self.actor {
            Actor::Admin => {
                "The admin token never makes a destructive change: a person makes it with the CLI or the web UI, or unlocks it for an agent token of level full".to_owned()
            }
            Actor::Token { .. } => {
                "A person changes a token's level, and unlocks destructive changes, on the Tokens page of the web UI or with davy token".to_owned()
            }
        };
        format!(
            "{tool} needs level {}, and {} holds {held}. {who}.",
            need.words(),
            describe(&self.actor)
        )
    }
}

fn describe(actor: &Actor) -> String {
    match actor {
        Actor::Admin => "the admin token".to_owned(),
        Actor::Token { name, .. } => format!("the token {name:?}"),
    }
}

#[cfg(test)]
#[path = "access_tests.rs"]
mod tests;
