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
    /// Where an item sits on the shopping-to-owning path. `Skipped` records a
    /// deliberate "considered and passed" so it doesn't resurface as a candidate.
    Status {
        Wishlist => "wishlist",
        Ordered  => "ordered",
        Owned    => "owned",
        Skipped  => "skipped",
    }
}

// `Status` is declared by `str_enum!`, which has no way to carry
// `#[derive(Default)]` + a `#[default]` variant marker; the manual impl is
// the macro's escape hatch.
#[allow(clippy::derivable_impls)]
impl Default for Status {
    fn default() -> Self {
        Status::Wishlist
    }
}

/// A section of the wardrobe (e.g. "Footwear"), optionally with a target count
/// of pieces to own and a line of guidance.
#[derive(Debug, Clone, Serialize)]
pub struct Category {
    pub id: i64,
    pub name: String,
    pub note: Option<String>,
    pub target_count: Option<i64>,
    pub sort_order: i64,
    pub created_at: String,
}

/// A single garment link and its shopping metadata.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: i64,
    pub category_id: i64,
    pub url: String,
    pub title: Option<String>,
    pub store: Option<String>,
    pub price_cents: Option<i64>,
    pub image_url: Option<String>,
    pub status: Status,
    pub size: Option<String>,
    pub notes: Option<String>,
    pub sort_order: i64,
    pub created_at: String,
    pub updated_at: String,
}
