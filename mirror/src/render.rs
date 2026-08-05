//! The public pages.
//!
//! Server-rendered HTML, no JavaScript, no client-side routing. A public page
//! has to work in a link preview, in a crawler, in a text browser, and on a
//! phone with a dying battery — an SPA satisfies none of those without a
//! second rendering stack, and this page has no interaction to justify one.
//!
//! Styling is TRITIUM (DG-002) with DG-001's rules intact, with two departures
//! worth naming:
//!
//! - **Rows, not a kanban.** DG-001 §5 is explicit that a dense, scannable
//!   table beats a grid of cards, and it is right here: the reader wants to
//!   read what is being worked on, not to drag anything. Each section renders
//!   as a table with the columns that carry information.
//! - **Theme follows the reader, not tide.** Every other fleet UI takes its
//!   dark/light state from the tide service. Visitors here have no account and
//!   no preference stored anywhere, so the page honours
//!   `prefers-color-scheme`, defaulting to TRITIUM's canonical dark.
//!
//! Fizzy's per-column colours are deliberately not carried over. In TRITIUM a
//! hue means something specific — amber is instrumentation, blue is
//! interactive, red is critical — and painting sections in arbitrary decorative
//! colours would leave the page unable to say any of those things.

use chrono::{DateTime, Utc};
use maud::{html, Markup, PreEscaped, DOCTYPE};

use crate::store::{Board, Card, SectionKind, SyncState};

/// Site-wide facts the templates need.
pub struct Site {
    /// Shown in the masthead and in link previews.
    pub name: String,
    /// The public origin, e.g. `https://board.deepwa7er.com`. Used only to
    /// build absolute URLs for `og:` tags and `<link rel=canonical>`, which
    /// have to be absolute to work.
    pub public_url: String,
}

impl Site {
    fn absolute(&self, path: &str) -> String {
        format!("{}{path}", self.public_url.trim_end_matches('/'))
    }
}

/// The board list. Only reached when more than one board is published — with
/// a single board, `/` is that board.
pub fn index(site: &Site, boards: &[Board], state: &SyncState) -> Markup {
    let body = html! {
        h1 .masthead__title { (site.name) }
        table .rows {
            thead {
                tr {
                    th { "BOARD" }
                    th .num { "CARDS" }
                }
            }
            tbody {
                @for board in boards {
                    tr {
                        td { a href=(format!("/b/{}", board.slug)) { (board.name) } }
                        td .num { (board.card_count()) }
                    }
                }
            }
        }
        @if boards.is_empty() {
            (nothing_published())
        }
    };
    page(
        site,
        &site.name,
        "A read-only mirror of a Fizzy board.",
        "/",
        body,
        state,
    )
}

/// One board: every section, every card, in one page. No pagination and no
/// "load more" — the whole point is that a reader can see the work at a
/// glance, and a board is small enough to say all of at once.
pub fn board(site: &Site, board: &Board, state: &SyncState, at_root: bool) -> Markup {
    let path = if at_root {
        "/".to_string()
    } else {
        format!("/b/{}", board.slug)
    };
    let body = html! {
        h1 .masthead__title { (board.name) }
        @if !board.description_html.is_empty() {
            div .prose .board__description { (PreEscaped(&board.description_html)) }
        }
        @for section in &board.sections {
            section .section {
                header .section__header {
                    h2 .section__title { (section.name.to_uppercase()) }
                    span .section__count { (section.cards.len()) }
                }
                @if section.cards.is_empty() {
                    p .section__empty { "— empty —" }
                } @else {
                    (card_table(board, section.kind, &section.cards))
                }
            }
        }
        @if board.sections.is_empty() {
            (nothing_published())
        }
    };
    let description = format!(
        "{} — {} cards, mirrored read-only.",
        board.name,
        board.card_count()
    );
    page(site, &board.name, &description, &path, body, state)
}

fn card_table(board: &Board, kind: SectionKind, cards: &[Card]) -> Markup {
    let activity_label = match kind {
        SectionKind::Closed => "CLOSED",
        _ => "UPDATED",
    };
    html! {
        table .rows {
            thead {
                tr {
                    th .num { "#" }
                    th { "TITLE" }
                    th { "TAGS" }
                    th { "PEOPLE" }
                    th .num { (activity_label) }
                }
            }
            tbody {
                @for card in cards {
                    tr {
                        td .num { (card.number) }
                        td {
                            @if card.golden {
                                // Amber is TRITIUM's instrumentation voice:
                                // this is the board's own "look here" flag.
                                span .golden title="Marked golden in Fizzy" { "★" }
                                " "
                            }
                            a href=(format!("/b/{}/c/{}", board.slug, card.number)) { (card.title) }
                        }
                        td .tags {
                            @for tag in &card.tags {
                                span .tag { (tag) }
                            }
                        }
                        td .people { (people(card)) }
                        td .num .when title=(&card.last_active_at) {
                            (relative(&card.last_active_at))
                        }
                    }
                }
            }
        }
    }
}

/// Who opened the card and who is on it. Plain text is the author; an arrow
/// marks an assignment.
///
/// A name is never printed twice. On a personal board almost every card is
/// opened and self-assigned by the same person, and "deepwater → deepwater" on
/// every row is noise that crowds out the rows either side of it. Identity is
/// compared by display name, which is sound because the display name is also
/// the only thing the page shows: two people the page renders identically are
/// two people the reader cannot tell apart anyway.
///
/// Avatars accompany names; they never replace them (DG-001 §5 — the least
/// useful information does not get to be the largest).
fn people(card: &Card) -> Markup {
    let author_is_assigned = card.assignees.iter().any(|a| a.name == card.creator.name);
    html! {
        span .person .person--assigned[author_is_assigned]
             title=(if author_is_assigned { "Opened this card and is assigned to it" } else { "Opened this card" }) {
            @if author_is_assigned { "→ " }
            @if let Some(avatar) = &card.creator.avatar {
                img .avatar src=(avatar) alt="" width="16" height="16";
            }
            (card.creator.name)
        }
        @for assignee in card.assignees.iter().filter(|a| a.name != card.creator.name) {
            span .person .person--assigned title="Assigned" {
                "→ "
                @if let Some(avatar) = &assignee.avatar {
                    img .avatar src=(avatar) alt="" width="16" height="16";
                }
                (assignee.name)
            }
        }
        @if card.more_assignees {
            span .person .person--assigned title="More assignees than Fizzy lists" { "→ …" }
        }
    }
}

/// One card, with its full text.
pub fn card(
    site: &Site,
    board: &Board,
    card: &Card,
    kind: SectionKind,
    state: &SyncState,
) -> Markup {
    let standing = match kind {
        SectionKind::Triage => "TRIAGE",
        SectionKind::Column => "IN A COLUMN",
        SectionKind::NotNow => "NOT NOW",
        SectionKind::Closed => "CLOSED",
    };
    let path = format!("/b/{}/c/{}", board.slug, card.number);
    let body = html! {
        nav .breadcrumb {
            a href=(format!("/b/{}", board.slug)) { (board.name) }
            span .breadcrumb__separator { "/" }
            span { "#" (card.number) }
        }
        h1 .masthead__title {
            @if card.golden { span .golden { "★" } " " }
            (card.title)
        }
        dl .meta {
            div .meta__field { dt { "STATUS" } dd { (standing) } }
            div .meta__field { dt { "OPENED" } dd title=(&card.created_at) { (stamp(&card.created_at)) } }
            div .meta__field { dt { "UPDATED" } dd title=(&card.last_active_at) { (stamp(&card.last_active_at)) } }
            div .meta__field { dt { "BY" } dd { (card.creator.name) } }
            @if !card.assignees.is_empty() {
                div .meta__field {
                    dt { "ASSIGNED" }
                    dd {
                        @for (index, assignee) in card.assignees.iter().enumerate() {
                            @if index > 0 { ", " }
                            (assignee.name)
                        }
                        @if card.more_assignees { ", …" }
                    }
                }
            }
            @if !card.tags.is_empty() {
                div .meta__field {
                    dt { "TAGS" }
                    dd .tags { @for tag in &card.tags { span .tag { (tag) } } }
                }
            }
        }
        @if let Some(image) = &card.image_path {
            figure .card__image { img src=(image) alt=(format!("Image on card {}", card.number)); }
        }
        @if card.description_html.is_empty() {
            p .section__empty { "— no description —" }
        } @else {
            div .prose { (PreEscaped(&card.description_html)) }
        }
    };
    let description = if card.description_html.is_empty() {
        format!("{} · card #{} on {}", card.title, card.number, board.name)
    } else {
        excerpt(&card.description_html, 200)
    };
    page(site, &card.title, &description, &path, body, state)
}

pub fn not_found(site: &Site, state: &SyncState) -> Markup {
    let body = html! {
        h1 .masthead__title { "404" }
        p { "No such board or card here." }
        p { a href="/" { "← back" } }
    };
    page(site, "404", "Not found.", "/", body, state)
}

fn nothing_published() -> Markup {
    html! {
        p .section__empty {
            "Nothing is published right now."
        }
    }
}

fn page(
    site: &Site,
    title: &str,
    description: &str,
    path: &str,
    body: Markup,
    state: &SyncState,
) -> Markup {
    let full_title = if title == site.name {
        site.name.clone()
    } else {
        format!("{title} · {}", site.name)
    };
    let canonical = site.absolute(path);
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (full_title) }
                meta name="description" content=(description);
                link rel="canonical" href=(canonical);
                meta property="og:type" content="website";
                meta property="og:title" content=(full_title);
                meta property="og:description" content=(description);
                meta property="og:url" content=(canonical);
                meta name="twitter:card" content="summary";
                // The stylesheet is inlined rather than linked: it is small,
                // it saves a round trip on the only request that matters, and
                // it sidesteps cache-busting a separate file forever.
                style { (PreEscaped(STYLE)) }
            }
            body {
                header .masthead {
                    a .masthead__home href="/" { (site.name) }
                    span .masthead__badge { "READ-ONLY MIRROR" }
                }
                main { (body) }
                (footer(state))
            }
        }
    }
}

/// Documentation furniture, and the honest part: the page says how fresh it
/// is instead of letting the reader assume it is live.
fn footer(state: &SyncState) -> Markup {
    let synced = match &state.last_success_at {
        Some(at) => format!("SYNCED {} ({})", stamp(at), relative(at)),
        None => "NEVER SYNCED".to_string(),
    };
    let stale = state
        .last_success_at
        .as_deref()
        .and_then(parse)
        .map(|at| (Utc::now() - at).num_hours() >= 6)
        .unwrap_or(true);
    html! {
        footer .footer {
            hr .footer__rule;
            div .footer__row {
                span .footer__stamp .footer__stamp--stale[stale] { (synced) }
                @if state.last_error.is_some() {
                    // Named, not hidden: a mirror that has stopped updating
                    // looks exactly like a board where nothing is happening.
                    // The reason stays in the log — a public page is the wrong
                    // place to describe someone's private infrastructure — but
                    // the time of the failed attempt is fair to show.
                    span .footer__warn title=(match &state.last_attempt_at {
                        Some(at) => format!("Last attempt {} did not reach the source", stamp(at)),
                        None => "The mirror could not reach the source".to_string(),
                    }) {
                        "· UPSTREAM UNREACHABLE"
                    }
                }
                span .wordmark { "fleet · mirror" }
            }
        }
    }
}

fn parse(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// `2026-08-05 19:38 UTC` — precise, sortable, unambiguous.
fn stamp(value: &str) -> String {
    match parse(value) {
        Some(at) => at.format("%Y-%m-%d %H:%M UTC").to_string(),
        // Never seen in practice; showing the raw value beats inventing one.
        None => value.to_string(),
    }
}

/// Coarse relative time. Always paired with the exact timestamp in a `title`,
/// so nothing is rounded away — only summarized.
fn relative(value: &str) -> String {
    let Some(at) = parse(value) else {
        return value.to_string();
    };
    let elapsed = Utc::now() - at;
    let minutes = elapsed.num_minutes();
    if minutes < 1 {
        "just now".into()
    } else if minutes < 60 {
        format!("{minutes}m ago")
    } else if elapsed.num_hours() < 24 {
        format!("{}h ago", elapsed.num_hours())
    } else if elapsed.num_days() < 30 {
        format!("{}d ago", elapsed.num_days())
    } else {
        format!("{}mo ago", elapsed.num_days() / 30)
    }
}

/// Plain-text excerpt of sanitized HTML, for `<meta name=description>`.
fn excerpt(html: &str, limit: usize) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            c if c.is_whitespace() => {
                if !text.ends_with(' ') {
                    text.push(' ');
                }
            }
            c => text.push(c),
        }
    }
    let text = text.trim();
    match text.char_indices().nth(limit) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text.to_string(),
    }
}

const STYLE: &str = r#"
:root {
  --bg:#000; --surface:#0a0a0a; --surface-2:#101010;
  --ink:#00a645; --ink-muted:#00753d; --ink-faint:#4a4a48;
  --rule:#242424; --rule-strong:#3d3d3b; --emphasis:#fff;
  --accent:#4a90d4; --warn:#ffbf00; --crit:#e0281e;
  --font-mono:"Berkeley Mono","JetBrains Mono","IBM Plex Mono",ui-monospace,monospace;
  --s1:4px; --s2:8px; --s3:12px; --s4:16px; --s5:24px; --s6:32px;
}
@media (prefers-color-scheme: light) {
  :root {
    --bg:#f4f3ee; --surface:#fafaf8; --surface-2:#eceae3;
    --ink:#00702f; --ink-muted:#4d7a5e; --ink-faint:#8a8678;
    --rule:#d8d6cd; --rule-strong:#bfbcb0; --emphasis:#1a1a1a;
    --accent:#2a6bb0; --warn:#a85d00; --crit:#b3231a;
  }
}
* { box-sizing:border-box; }
html { -webkit-text-size-adjust:100%; }
body {
  margin:0; background:var(--bg); color:var(--ink);
  font:14px/1.45 var(--font-mono); letter-spacing:-0.01em;
  font-variant-numeric:tabular-nums;
}
main { max-width:1100px; margin:0 auto; padding:0 var(--s4) var(--s6); }
a { color:var(--accent); text-decoration:none; }
a:hover { text-decoration:underline; }
:focus-visible { outline:2px solid var(--accent); outline-offset:1px; }
hr { border:0; border-top:1px solid var(--rule); margin:var(--s5) 0; }

.masthead {
  display:flex; align-items:baseline; gap:var(--s3); flex-wrap:wrap;
  max-width:1100px; margin:0 auto; padding:var(--s4);
  border-bottom:1px solid var(--rule);
}
.masthead__home { color:var(--emphasis); font-weight:700; letter-spacing:0.02em; }
.masthead__badge {
  color:var(--ink-muted); border:1px solid var(--rule-strong);
  padding:1px var(--s2); font-size:11px; letter-spacing:0.08em;
}
.masthead__title {
  font-size:20px; font-weight:700; color:var(--emphasis);
  margin:var(--s5) 0 var(--s4); letter-spacing:0;
}

.breadcrumb { margin-top:var(--s4); color:var(--ink-muted); font-size:12px; }
.breadcrumb__separator { padding:0 var(--s2); color:var(--ink-faint); }

.section { margin-bottom:var(--s6); }
.section__header {
  display:flex; align-items:baseline; gap:var(--s2);
  border-bottom:1px solid var(--rule-strong); padding-bottom:var(--s1);
}
.section__title { font-size:12px; letter-spacing:0.1em; margin:0; color:var(--emphasis); }
.section__count { color:var(--ink-faint); font-size:12px; }
.section__empty { color:var(--ink-faint); margin:var(--s3) 0; }

.rows { width:100%; border-collapse:collapse; margin-top:var(--s2); }
.rows th {
  text-align:left; font-size:11px; letter-spacing:0.08em; font-weight:400;
  color:var(--ink-faint); padding:var(--s2) var(--s3); border-bottom:1px solid var(--rule);
  white-space:nowrap;
}
.rows td { padding:var(--s2) var(--s3); border-bottom:1px solid var(--rule); vertical-align:top; }
.rows tr:hover td { background:var(--surface-2); }
.rows .num { text-align:right; white-space:nowrap; }
.rows th.num { text-align:right; }
.when { color:var(--ink-muted); }
.golden { color:var(--warn); }

.tags { line-height:1.8; }
.tag {
  border:1px solid var(--rule-strong); color:var(--ink-muted);
  padding:0 var(--s1); margin-right:var(--s1); font-size:12px; white-space:nowrap;
}
.people { color:var(--ink-muted); font-size:12px; }
.person { display:inline-flex; align-items:center; gap:var(--s1); margin-right:var(--s2); white-space:nowrap; }
.person--assigned { color:var(--ink); }
.avatar { border:1px solid var(--rule); object-fit:cover; }

.meta { display:flex; flex-wrap:wrap; gap:var(--s2) var(--s5); margin:0 0 var(--s5); padding:var(--s3);
        border:1px solid var(--rule); background:var(--surface); }
.meta__field { display:flex; gap:var(--s2); align-items:baseline; }
.meta dt { color:var(--ink-faint); font-size:11px; letter-spacing:0.08em; }
.meta dd { margin:0; color:var(--ink); }

.card__image { margin:0 0 var(--s5); }
.card__image img { max-width:100%; height:auto; border:1px solid var(--rule); }

.prose { max-width:72ch; }
.prose p { margin:0 0 var(--s3); }
.prose h1,.prose h2,.prose h3,.prose h4 { color:var(--emphasis); font-size:15px; margin:var(--s5) 0 var(--s2); }
.prose ul,.prose ol { padding-left:var(--s5); margin:0 0 var(--s3); }
.prose li { margin-bottom:var(--s1); }
.prose blockquote { margin:0 0 var(--s3); padding-left:var(--s3); border-left:2px solid var(--rule-strong); color:var(--ink-muted); }
.prose pre { background:var(--surface); border:1px solid var(--rule); padding:var(--s3); overflow-x:auto; }
.prose code { color:var(--ink-muted); }
.prose img { max-width:100%; height:auto; border:1px solid var(--rule); }
.prose table { border-collapse:collapse; }
.prose th,.prose td { border:1px solid var(--rule); padding:var(--s1) var(--s2); }
.board__description { margin-bottom:var(--s5); color:var(--ink-muted); }

.footer { max-width:1100px; margin:0 auto; padding:0 var(--s4) var(--s5); }
.footer__rule { margin:0 0 var(--s2); }
.footer__row { display:flex; flex-wrap:wrap; gap:var(--s3); align-items:baseline; font-size:11px; letter-spacing:0.06em; }
.footer__stamp { color:var(--ink-faint); }
.footer__stamp--stale { color:var(--warn); }
.footer__warn { color:var(--warn); }
.wordmark { margin-left:auto; font-style:italic; letter-spacing:0.12em; color:var(--accent); opacity:0.85; }

@media (max-width:640px) {
  .rows th:nth-child(3), .rows td:nth-child(3),
  .rows th:nth-child(4), .rows td:nth-child(4) { display:none; }
  main { padding:0 var(--s3) var(--s5); }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Person;

    fn site() -> Site {
        Site {
            name: "board".into(),
            public_url: "https://board.example.com".into(),
        }
    }

    fn state() -> SyncState {
        SyncState {
            last_success_at: Some(Utc::now().to_rfc3339()),
            last_attempt_at: Some(Utc::now().to_rfc3339()),
            last_error: None,
        }
    }

    fn board_with(card: Card) -> Board {
        Board {
            id: "b".into(),
            slug: "playground".into(),
            name: "Playground".into(),
            description_html: String::new(),
            sections: vec![crate::store::Section {
                kind: SectionKind::Triage,
                name: "Triage".into(),
                cards: vec![card],
            }],
        }
    }

    fn plain_card() -> Card {
        Card {
            number: 7,
            title: "a title".into(),
            description_html: "<p>body</p>".into(),
            image_path: None,
            tags: vec![],
            golden: false,
            created_at: "2026-08-01T10:00:00Z".into(),
            last_active_at: "2026-08-02T10:00:00Z".into(),
            creator: Person {
                name: "deepwater".into(),
                avatar: None,
            },
            assignees: vec![],
            more_assignees: false,
        }
    }

    #[test]
    fn escapes_card_text() {
        let mut hostile = plain_card();
        hostile.title = "<script>alert(1)</script>".into();
        let html = board_html(&board_with(hostile), &state());
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }

    #[test]
    fn card_page_carries_link_preview_tags() {
        let mirrored = board_with(plain_card());
        let (found, kind) = mirrored.card(7).unwrap();
        let html = super::card(&site(), &mirrored, found, kind, &state()).into_string();
        assert!(html.contains(r#"property="og:title""#), "{html}");
        assert!(
            html.contains(r#"href="https://board.example.com/b/playground/c/7""#),
            "{html}"
        );
    }

    #[test]
    fn says_when_it_last_synced() {
        let board = board_with(plain_card());
        let never = SyncState::default();
        assert!(board_html(&board, &never).contains("NEVER SYNCED"));

        let stale = SyncState {
            last_success_at: Some((Utc::now() - chrono::Duration::hours(30)).to_rfc3339()),
            ..SyncState::default()
        };
        // Match the emitted class attribute, not the bare name: the
        // stylesheet is inlined into every page, so the selector itself is
        // always present in the document and would make this assertion pass
        // no matter what the footer did.
        let worn = r#"class="footer__stamp footer__stamp--stale""#;
        assert!(board_html(&board, &stale).contains(worn));
        assert!(!board_html(&board, &state()).contains(worn));
        assert!(board_html(&board, &never).contains(worn));
    }

    #[test]
    fn flags_an_unreachable_upstream() {
        let board = board_with(plain_card());
        let broken = SyncState {
            last_success_at: Some(Utc::now().to_rfc3339()),
            last_attempt_at: Some(Utc::now().to_rfc3339()),
            last_error: Some("connection refused".into()),
        };
        let html = board_html(&board, &broken);
        assert!(html.contains("UPSTREAM UNREACHABLE"), "{html}");
        // The reason itself stays in the log, not on a public page.
        assert!(!html.contains("connection refused"), "{html}");
    }

    fn board_html(b: &Board, state: &SyncState) -> String {
        super::board(&site(), b, state, true).into_string()
    }

    #[test]
    fn never_prints_the_same_person_twice() {
        let mut self_assigned = plain_card();
        self_assigned.assignees = vec![Person {
            name: "deepwater".into(),
            avatar: None,
        }];
        let html = board_html(&board_with(self_assigned), &state());
        assert_eq!(html.matches("deepwater").count(), 1, "{html}");

        let mut delegated = plain_card();
        delegated.assignees = vec![Person {
            name: "someone".into(),
            avatar: None,
        }];
        let html = board_html(&board_with(delegated), &state());
        assert!(html.contains("deepwater"), "the author is still named");
        assert!(html.contains("someone"), "the assignee is still named");
    }

    #[test]
    fn excerpts_are_plain_text() {
        assert_eq!(excerpt("<p>hello <b>there</b></p>", 200), "hello there");
        assert_eq!(excerpt("<p>abcdef</p>", 3), "abc…");
    }
}
