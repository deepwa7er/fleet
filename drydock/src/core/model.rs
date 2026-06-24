use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Macro to define a string-backed enum that round-trips through SQLite TEXT,
/// serde (as the wire string), and Display — with a single source of truth for
/// the string spellings used in the schema's CHECK constraints.
macro_rules! str_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $( #[serde(rename = $s)] $variant ),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $( $name::$variant => $s ),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, String> {
                match s {
                    $( $s => Ok($name::$variant), )+
                    other => Err(format!(concat!("invalid ", stringify!($name), ": {}"), other)),
                }
            }
        }
    };
}

str_enum! {
    /// Lifecycle state. Transitions are governed by `core::state`. `Done` (merged
    /// + deployed) and `Closed` (abandoned by the human) are both terminal.
    State {
        Open => "open",
        InProgress => "in-progress",
        NeedsInput => "needs-input",
        InReview => "in-review",
        Blocked => "blocked",
        Done => "done",
        Closed => "closed",
    }
}

str_enum! {
    Priority { High => "high", Med => "med", Low => "low" }
}

str_enum! {
    TicketType { Feature => "feature" }
}

str_enum! {
    Author { Worker => "worker", Human => "human" }
}

str_enum! {
    CommentKind { Note => "note", Question => "question", Answer => "answer" }
}

#[derive(Debug, Clone, Serialize)]
pub struct Ticket {
    pub id: i64,
    pub title: String,
    #[serde(rename = "type")]
    pub ticket_type: TicketType,
    pub target: String,
    pub state: State,
    pub priority: Priority,
    pub goal: String,
    pub acceptance: Option<String>,
    pub constraints: Option<String>,
    pub branch: Option<String>,
    pub pr_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comment {
    pub id: i64,
    pub ticket_id: i64,
    pub author: Author,
    pub kind: CommentKind,
    pub body: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub id: i64,
    pub ticket_id: i64,
    pub from_state: Option<State>,
    pub to_state: State,
    pub actor: Author,
    pub detail: Option<String>,
    pub created_at: String,
}

/// A ticket with its full thread and audit trail — the detail view.
#[derive(Debug, Clone, Serialize)]
pub struct TicketDetail {
    #[serde(flatten)]
    pub ticket: Ticket,
    pub comments: Vec<Comment>,
    pub events: Vec<Event>,
}
