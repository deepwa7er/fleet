//! The sync pass: pull the published boards from Fizzy and replace the
//! snapshot.
//!
//! A pass is all-or-nothing. If any request fails part-way through — the
//! laptop Fizzy runs on went to sleep, the tailnet blipped — the pass returns
//! an error and the previous snapshot stays exactly as it was. A public page
//! showing yesterday's board is useful; a public page showing half of today's
//! board is a bug that looks like data.
//!
//! What gets mirrored is decided in one place, [`Client::boards`] plus the
//! `is_published` filter below: a board is mirrored if and only if Fizzy says
//! it is published, and a card is mirrored if and only if it is not a draft.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::assets::{Cache, Stripped};
use crate::fizzy::{self, Client, Standing};
use crate::sanitize;
use crate::store::{Board, Card, Person, Section, SectionKind, Store};

/// What one pass did, for `mirror sync` and the log line.
#[derive(Debug, Default, Clone, Copy)]
pub struct Pass {
    pub boards: usize,
    pub cards: usize,
    pub images: usize,
    /// Images stored with their metadata intact because the format is not one
    /// [`crate::assets`] can take apart. Worth saying out loud: those carry
    /// whatever EXIF the camera wrote.
    pub unstripped: usize,
    pub assets_removed: usize,
}

pub struct Deps {
    pub store: Arc<Store>,
    pub client: Arc<Client>,
    pub cache: Arc<Cache>,
}

/// Run a pass now, then every `interval`, forever.
pub async fn run(deps: Deps, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    // Missed ticks are not worth catching up on: the next pass reads the
    // current state of the board regardless, so bursting after a long stall
    // would only hammer Fizzy to reach the same answer.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match once(&deps).await {
            Ok(pass) => tracing::info!(
                boards = pass.boards,
                cards = pass.cards,
                images = pass.images,
                assets_removed = pass.assets_removed,
                "sync complete"
            ),
            Err(e) => tracing::warn!("sync failed: {e:#}"),
        }
    }
}

/// One pass. Records its own outcome in the store either way, so the page can
/// report how stale it is.
pub async fn once(deps: &Deps) -> Result<Pass> {
    let at = chrono::Utc::now().to_rfc3339();
    match collect(deps).await {
        Ok((boards, mut pass)) => {
            deps.store
                .replace(&boards)
                .context("storing the snapshot")?;
            let keep = deps
                .store
                .referenced_assets()
                .context("listing referenced assets")?;
            pass.assets_removed = deps
                .cache
                .retain(&keep)
                .context("pruning the asset cache")?;
            deps.store
                .record_success(&at)
                .context("recording success")?;
            Ok(pass)
        }
        Err(e) => {
            // Best-effort: if recording the failure also fails, the original
            // error is the one worth reporting.
            let _ = deps.store.record_failure(&at, &format!("{e:#}"));
            Err(e)
        }
    }
}

async fn collect(deps: &Deps) -> Result<(Vec<Board>, Pass)> {
    let mut pass = Pass::default();
    let mut images = Images::new(deps);
    let mut slugs = Slugs::default();
    let mut boards = Vec::new();

    let published = deps
        .client
        .boards()
        .await
        .context("listing boards")?
        .into_iter()
        .filter(fizzy::Board::is_published);

    for board in published {
        let mut sections = Vec::new();

        let triage = deps
            .client
            .standing_cards(&board.id, Standing::Triage)
            .await
            .with_context(|| format!("listing triage cards for board {}", board.name))?;
        push_section(&mut sections, SectionKind::Triage, "Triage", triage);

        for column in deps
            .client
            .columns(&board.id)
            .await
            .with_context(|| format!("listing columns for board {}", board.name))?
        {
            let cards = deps
                .client
                .column_cards(&board.id, &column.id)
                .await
                .with_context(|| format!("listing cards in column {}", column.name))?;
            // Named columns are kept even when empty: an empty "Doing" is a
            // fact about the work, not an absence of information.
            sections.push((SectionKind::Column, column.name, cards));
        }

        let not_now = deps
            .client
            .standing_cards(&board.id, Standing::NotNow)
            .await
            .with_context(|| format!("listing postponed cards for board {}", board.name))?;
        push_section(&mut sections, SectionKind::NotNow, "Not now", not_now);

        let closed = deps
            .client
            .standing_cards(&board.id, Standing::Closed)
            .await
            .with_context(|| format!("listing closed cards for board {}", board.name))?;
        push_section(&mut sections, SectionKind::Closed, "Closed", closed);

        let mut stored_sections = Vec::new();
        for (kind, name, cards) in sections {
            let mut stored_cards = Vec::new();
            for card in cards.into_iter().filter(fizzy::Card::is_published) {
                stored_cards.push(convert(card, &mut images).await?);
                pass.cards += 1;
            }
            stored_sections.push(Section {
                kind,
                name,
                cards: stored_cards,
            });
        }

        let description_html = images
            .clean(board.public_description_html.as_deref().unwrap_or_default())
            .await?;

        boards.push(Board {
            slug: slugs.take(&board.name),
            id: board.id,
            name: board.name,
            description_html,
            sections: stored_sections,
        });
        pass.boards += 1;
    }

    pass.images = images.stored;
    pass.unstripped = images.unstripped;
    Ok((boards, pass))
}

fn push_section(
    sections: &mut Vec<(SectionKind, String, Vec<fizzy::Card>)>,
    kind: SectionKind,
    name: &str,
    cards: Vec<fizzy::Card>,
) {
    // Unlike a named column, these three are not places the user made: an
    // empty "Closed" heading is furniture with nothing behind it.
    if !cards.is_empty() {
        sections.push((kind, name.to_string(), cards));
    }
}

async fn convert(card: fizzy::Card, images: &mut Images<'_>) -> Result<Card> {
    let image_path = match &card.image_url {
        Some(url) => images.fetch(url).await?,
        None => None,
    };
    let description_html = images.clean(&card.description_html).await?;
    let creator = images.person(card.creator).await?;
    let mut assignees = Vec::new();
    for assignee in card.assignees {
        assignees.push(images.person(assignee).await?);
    }

    Ok(Card {
        number: card.number,
        title: card.title,
        description_html,
        image_path,
        tags: card.tags,
        golden: card.golden,
        created_at: card.created_at,
        last_active_at: card.last_active_at,
        creator,
        assignees,
        more_assignees: card.has_more_assignees,
    })
}

/// Fetches images once each and remembers the answer.
///
/// Avatars repeat on every card, so without this a nineteen-card board would
/// re-download the same portrait nineteen times. A `None` answer is cached
/// too: a foreign origin or a broken URL should be refused once, not retried
/// on every card that mentions it.
struct Images<'a> {
    deps: &'a Deps,
    seen: HashMap<String, Option<String>>,
    stored: usize,
    unstripped: usize,
}

impl<'a> Images<'a> {
    fn new(deps: &'a Deps) -> Self {
        Self {
            deps,
            seen: HashMap::new(),
            stored: 0,
            unstripped: 0,
        }
    }

    async fn fetch(&mut self, url: &str) -> Result<Option<String>> {
        if let Some(known) = self.seen.get(url) {
            return Ok(known.clone());
        }
        let fetched = self.deps.client.fetch_asset(url).await;
        let path = match fetched {
            Ok(Some((bytes, content_type))) => {
                match self.deps.cache.store(&bytes, content_type.as_deref())? {
                    Some((path, stripped)) => {
                        self.stored += 1;
                        if stripped == Stripped::UnknownFormat {
                            self.unstripped += 1;
                            tracing::warn!(
                                "{url} is a format whose metadata cannot be stripped; \
                                 it is published with whatever the camera wrote"
                            );
                        }
                        Some(path)
                    }
                    // Not an image (Fizzy's SVG initials avatar, most often).
                    None => None,
                }
            }
            // A foreign origin — refused by the client, not an error.
            Ok(None) => None,
            // One unreachable image must not sink the pass; the page renders
            // without it, which is what a browser would have shown anyway.
            Err(e) => {
                tracing::warn!("could not fetch {url}: {e:#}");
                None
            }
        };
        self.seen.insert(url.to_string(), path.clone());
        Ok(path)
    }

    async fn person(&mut self, user: fizzy::User) -> Result<Person> {
        let avatar = self.fetch(&user.avatar_url).await?;
        Ok(Person {
            name: user.name,
            avatar,
        })
    }

    /// Sanitize rich text, caching every image it references first.
    async fn clean(&mut self, html: &str) -> Result<String> {
        if html.trim().is_empty() {
            return Ok(String::new());
        }
        let mut resolved = HashMap::new();
        for url in sanitize::image_urls(html) {
            if let Some(path) = self.fetch(&url).await? {
                resolved.insert(url, path);
            }
        }
        Ok(sanitize::clean(html, &resolved, self.deps.client.origin()))
    }
}

/// Board slugs, unique within a snapshot.
#[derive(Default)]
struct Slugs {
    taken: HashSet<String>,
}

impl Slugs {
    fn take(&mut self, name: &str) -> String {
        let base = slugify(name);
        if self.taken.insert(base.clone()) {
            return base;
        }
        // Two boards with the same name is unusual but not an error, and one
        // of them silently vanishing behind a UNIQUE constraint would be.
        for suffix in 2..1000 {
            let candidate = format!("{base}-{suffix}");
            if self.taken.insert(candidate.clone()) {
                return candidate;
            }
        }
        unreachable!("a thousand boards sharing one name is not a naming problem")
    }
}

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        // A board named entirely in emoji still needs an address.
        "board".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_url_safe() {
        assert_eq!(slugify("Playground"), "playground");
        assert_eq!(slugify("Q3 — Roadmap & Ideas!"), "q3-roadmap-ideas");
        assert_eq!(slugify("  spaced  out  "), "spaced-out");
        assert_eq!(slugify("🌊"), "board");
    }

    #[test]
    fn slugs_stay_unique_within_a_snapshot() {
        let mut slugs = Slugs::default();
        assert_eq!(slugs.take("Ideas"), "ideas");
        assert_eq!(slugs.take("Ideas"), "ideas-2");
        assert_eq!(slugs.take("ideas!"), "ideas-3");
    }
}
