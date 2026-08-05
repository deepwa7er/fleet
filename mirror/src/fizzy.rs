//! The Fizzy read API client — and the exposure boundary.
//!
//! # The types here ARE the privacy filter
//!
//! Fizzy's API is an admin API: it answers with everything the token's user can
//! see. `users/_user.json.jbuilder` includes `email_address`; every record
//! carries `url` fields pointing back at the internal host; `boards.json` lists
//! private boards alongside published ones.
//!
//! None of that is filtered later, in the renderer, where a missed field is a
//! leak. It is filtered *here*, by omission: these structs simply have no field
//! for an email address, so serde discards it at the parse boundary and no
//! value of any type in this program can carry one. What the mirror cannot
//! parse, it cannot store, and what it cannot store it cannot serve.
//!
//! Two rules follow the same reasoning and are enforced below:
//!
//! - **Only same-origin URLs are ever fetched** ([`Client::fetch_asset`]).
//!   Card rich text can contain an `<img>` pointing anywhere, and a fetcher
//!   that honours it is an SSRF hole in a service that sits on a private
//!   tailnet. Foreign images are dropped, never proxied and never hot-linked.
//! - **Pagination follows the `Link` header only within the configured base.**
//!
//! Endpoints are account-prefixed (`/1/boards.json`): Fizzy "mounts" itself
//! under an account slug in `AccountSlug::Extractor` middleware, and without
//! the prefix every request 302s to the sign-in menu rather than failing
//! usefully.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;

/// A board, as much of one as the mirror is willing to know.
#[derive(Debug, Deserialize)]
pub struct Board {
    /// Fizzy's id. An internal join key only — never rendered, never in a URL.
    pub id: String,
    pub name: String,
    /// Serialized by Fizzy **only when the board is published**
    /// (`_board.json.jbuilder` guards it with `if board.published?`). Its
    /// presence, not its value, is what authorizes mirroring: publishing in
    /// Fizzy is the single control, so the toggle the user already knows is
    /// the one that governs. The value itself is discarded — it addresses the
    /// internal host, which is useless and undesirable on a public page.
    #[serde(default)]
    pub public_url: Option<String>,
    /// The board's public blurb, as rich text. Sanitized before storage.
    #[serde(default)]
    pub public_description_html: Option<String>,
}

impl Board {
    pub fn is_published(&self) -> bool {
        self.public_url.is_some()
    }
}

#[derive(Debug, Deserialize)]
pub struct Column {
    pub id: String,
    pub name: String,
}

/// A person, reduced to what a public card can show: a display name and the
/// avatar endpoint. No id, no email address, no role, no profile URL.
#[derive(Debug, Deserialize)]
pub struct User {
    pub name: String,
    pub avatar_url: String,
}

#[derive(Debug, Deserialize)]
pub struct Card {
    /// The per-account card number. This is the only card identifier the
    /// mirror publishes, and it is the one Fizzy itself puts in URLs.
    pub number: i64,
    pub title: String,
    /// `drafted` or `published`. Drafts are excluded at ingest.
    pub status: String,
    /// ActionText's raw body — NOT the sanitized render path. Must go through
    /// [`crate::sanitize`] before it is stored.
    #[serde(default)]
    pub description_html: String,
    #[serde(default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    // `closed` and `postponed` are in the JSON but deliberately not read: a
    // card's standing is already decided by which of Fizzy's four disjoint
    // card sets returned it, and two sources for one fact is one too many.
    pub golden: bool,
    pub created_at: String,
    pub last_active_at: String,
    pub creator: User,
    #[serde(default)]
    pub assignees: Vec<User>,
    /// Fizzy caps the serialized assignee list at five and sets this when more
    /// exist, so the card can say "+2" rather than silently under-report.
    #[serde(default)]
    pub has_more_assignees: bool,
}

impl Card {
    pub fn is_published(&self) -> bool {
        self.status == "published"
    }
}

/// The three card sets a Fizzy board has besides its named columns. They are
/// disjoint from the columns and from each other: a column's `cards.json`
/// serves `cards.active`, which excludes closed and postponed cards, and
/// triage is by definition the cards not yet in any column.
#[derive(Debug, Clone, Copy)]
pub enum Standing {
    /// Awaiting triage — Fizzy calls this the "stream".
    Triage,
    NotNow,
    Closed,
}

impl Standing {
    /// The path segment Fizzy uses (`namespace :columns` in routes.rb).
    fn segment(self) -> &'static str {
        match self {
            Standing::Triage => "stream",
            Standing::NotNow => "not_now",
            Standing::Closed => "closed",
        }
    }
}

pub struct Client {
    http: reqwest::Client,
    /// Origin + account prefix, without a trailing slash, e.g.
    /// `https://fizzy.intern.deepwa7er.net/1`.
    base: String,
    /// Origin alone, for the same-origin check on assets.
    origin: String,
    token: String,
}

impl Client {
    /// `base` is Fizzy's origin (`https://fizzy.intern.deepwa7er.net`);
    /// `account` is the numeric account slug from its URL prefix.
    pub fn new(base: &str, account: &str, token: String, timeout: Duration) -> Result<Self> {
        let origin = base.trim_end_matches('/').to_string();
        if !origin.starts_with("https://") && !origin.starts_with("http://") {
            bail!("fizzy base must be an absolute http(s) URL, got {origin:?}");
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(concat!("fleet-mirror/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            http,
            base: format!("{origin}/{}", account.trim_matches('/')),
            origin,
            token,
        })
    }

    /// Fizzy's origin. The renderer never emits it; [`crate::sanitize`] uses
    /// it to recognize — and unwrap — links that point back inside.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    async fn get(&self, url: &str) -> Result<reqwest::Response> {
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        if status.is_redirection() {
            // Fizzy answers an unauthenticated or account-less request with a
            // 302 to the sign-in menu rather than a 401, so a bad token looks
            // like success to anything that only checks `is_success`.
            bail!(
                "GET {url} redirected ({status}) — token rejected, or the account prefix is wrong"
            );
        }
        if !status.is_success() {
            bail!("GET {url} failed: {status}");
        }
        Ok(response)
    }

    /// Fetch every page of a collection, following geared_pagination's
    /// `Link: <…>; rel="next"` header. The next URL is checked against the
    /// configured origin before it is followed.
    async fn get_paged<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>> {
        let mut url = format!("{}{path}", self.base);
        let mut all = Vec::new();
        // A board with more pages than this is not a board, it is a runaway
        // loop; the cap turns that into a loud error instead of a hang.
        for _ in 0..200 {
            let response = self.get(&url).await?;
            let next = next_page_url(response.headers());
            let page: Vec<T> = response
                .json()
                .await
                .with_context(|| format!("parsing the response to GET {url}"))?;
            all.extend(page);
            match next {
                Some(next) if next.starts_with(&self.origin) => url = next,
                Some(next) => bail!("pagination tried to leave {}: {next}", self.origin),
                None => return Ok(all),
            }
        }
        bail!("pagination did not terminate for {path}")
    }

    pub async fn boards(&self) -> Result<Vec<Board>> {
        self.get_paged("/boards.json").await
    }

    pub async fn columns(&self, board_id: &str) -> Result<Vec<Column>> {
        self.get_paged(&format!("/boards/{board_id}/columns.json"))
            .await
    }

    pub async fn column_cards(&self, board_id: &str, column_id: &str) -> Result<Vec<Card>> {
        self.get_paged(&format!(
            "/boards/{board_id}/columns/{column_id}/cards.json"
        ))
        .await
    }

    pub async fn standing_cards(&self, board_id: &str, standing: Standing) -> Result<Vec<Card>> {
        self.get_paged(&format!(
            "/boards/{board_id}/columns/{}.json",
            standing.segment()
        ))
        .await
    }

    /// Fetch an image (avatar, card image, or rich-text attachment).
    ///
    /// Returns `Ok(None)` for anything not on Fizzy's own origin. That is the
    /// SSRF guard: card rich text is user-authored HTML and can name any host,
    /// and this process can reach a private tailnet. Foreign images are
    /// dropped from the page rather than proxied — hot-linking them instead
    /// would leak every visitor's IP to a third party and let that third party
    /// change what the page shows after the fact.
    pub async fn fetch_asset(&self, url: &str) -> Result<Option<(Vec<u8>, Option<String>)>> {
        if !url.starts_with(&self.origin) {
            return Ok(None);
        }
        let response = self.get(url).await?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_string());
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("reading the body of GET {url}"))?;
        Ok(Some((bytes.to_vec(), content_type)))
    }
}

/// Pull the `rel="next"` URL out of a `Link` header.
fn next_page_url(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let value = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    // `<https://…?page=2>; rel="next"`, possibly several comma-separated.
    value.split(',').find_map(|link| {
        let (target, params) = link.split_once(';')?;
        if !params.replace(' ', "").contains("rel=\"next\"") {
            return None;
        }
        let target = target.trim();
        Some(
            target
                .strip_prefix('<')?
                .strip_suffix('>')?
                .trim()
                .to_string(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, LINK};

    fn headers(link: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(LINK, HeaderValue::from_str(link).unwrap());
        h
    }

    #[test]
    fn reads_the_next_page_link() {
        let h =
            headers("<https://fizzy.example/1/boards/x/columns/stream.json?page=2>; rel=\"next\"");
        assert_eq!(
            next_page_url(&h).as_deref(),
            Some("https://fizzy.example/1/boards/x/columns/stream.json?page=2")
        );
    }

    #[test]
    fn ignores_other_relations() {
        let h = headers(
            "<https://fizzy.example/a>; rel=\"prev\", <https://fizzy.example/b>; rel=\"next\"",
        );
        assert_eq!(
            next_page_url(&h).as_deref(),
            Some("https://fizzy.example/b")
        );
        let h = headers("<https://fizzy.example/a>; rel=\"prev\"");
        assert_eq!(next_page_url(&h), None);
    }

    #[test]
    fn no_link_header_means_one_page() {
        assert_eq!(next_page_url(&HeaderMap::new()), None);
    }
}
