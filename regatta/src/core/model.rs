use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Every proposal is a course of exactly this many steps — the game is
/// "vote on ten". Enforced by the store on creation.
pub const SEQUENCE_LEN: usize = 10;

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
    /// The section of the activity catalog an activity belongs to. Purely for
    /// grouping in the picker — steps mix categories freely.
    Category {
        Foods      => "foods",
        Misc       => "misc",
        Physical   => "physical",
        VideoGames => "video-games",
    }
}

/// One entry in the activity catalog — a thing a step can demand, with the
/// unit its quantity is counted in (e.g. "Distance run" in "miles").
#[derive(Debug, Clone, Serialize)]
pub struct Activity {
    pub id: i64,
    pub name: String,
    pub category: Category,
    pub unit: String,
    pub sort_order: i64,
    pub created_at: String,
}

/// One step of a proposed course: an activity plus how much of it. The
/// activity's display fields are denormalized in so the list view is one
/// request.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub position: i64,
    pub activity_id: i64,
    pub activity: String,
    pub category: Category,
    pub unit: String,
    pub quantity: f64,
}

/// A proposed course with its tally. `voted` is whether the requesting voter
/// (the `?voter=` query, if any) has cast a vote — false when unknown.
#[derive(Debug, Clone, Serialize)]
pub struct Proposal {
    pub id: i64,
    pub title: String,
    pub author: String,
    pub created_at: String,
    pub votes: i64,
    pub voted: bool,
    pub steps: Vec<Step>,
}
