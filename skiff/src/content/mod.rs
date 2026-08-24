//! Message content as typed blocks (DW-004 §8).
//!
//! Rust never emits markup. Content crosses the boundary as a typed block
//! list with pre-resolved highlight tokens; React renders each block as a
//! component. That buys four things at once, which is why it beats both
//! obvious alternatives:
//!
//! - **Parsed once, at ingest.** Entries are immutable — a session file is
//!   append-only — so their parsed content is immutable too, and there is no
//!   cache to invalidate. Shipping raw markdown would mean re-parsing on every
//!   render, including every streaming flush.
//! - **Tail-only invalidation while streaming.** Only the last block changes
//!   as text arrives, so only the last block need be re-sent.
//! - **Code blocks stay real components** — copy, collapse, open-in-editor —
//!   which server-rendered HTML forfeits.
//! - **It is typed**, which is where "typed end to end" actually cashes out.
//!
//! **Raw HTML in the source is literal text, never markup.** LLM output is the
//! one place model text becomes page content; treating `<script>` as four
//! characters rather than an element is a property of the parser here, not a
//! discipline the renderer has to keep remembering.

mod highlight;
mod markdown;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use markdown::parse;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub enum Block {
    Paragraph { inlines: Vec<Inline> },
    Heading { level: u8, inlines: Vec<Inline> },
    Code { lang: Option<String>, tokens: Vec<Token> },
    List {
        ordered: bool,
        /// The first number of an ordered list, when it is not 1.
        #[ts(type = "number | null")]
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Quote { blocks: Vec<Block> },
    Table { head: Vec<Vec<Inline>>, rows: Vec<Vec<Vec<Inline>>> },
    Rule,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub enum Inline {
    Text { text: String },
    /// Inline literal. Never highlighted — a span of code with no language is
    /// a name, not a program.
    Code { text: String },
    Emph { inlines: Vec<Inline> },
    Strong { inlines: Vec<Inline> },
    Strike { inlines: Vec<Inline> },
    Link { href: String, inlines: Vec<Inline> },
    /// A hard line break within a paragraph. Soft breaks become spaces, since
    /// the renderer wraps.
    Break,
}

/// One run of highlighted source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub struct Token {
    pub class: TokenClass,
    pub text: String,
}

/// The five roles DW-001 §8 allows, and no more.
///
/// The guide is explicit that highlighting reuses the app's status palette
/// rather than introducing a second one: keywords take the single accent,
/// strings and inserted lines take `--good`, deleted lines take `--danger`,
/// comments take muted ink, and *everything else stays the block's ink*. A
/// six-colour scheme would be a second palette, so this enum stays closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub enum TokenClass {
    Keyword,
    Str,
    Comment,
    Deleted,
    Inserted,
    /// The block's own ink. Rendered unspanned.
    Plain,
}
