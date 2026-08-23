//! Fizzy (Once) API client — the write counterpart to `mirror::fizzy`.
//!
//! `mirror` owns a read-only client that deliberately has no field for
//! `email_address`, no POST, and a same-origin pagination guard. This crate
//! owns the write path: `POST /1/boards/:id/cards.json` with a `write` token.
//! It reuses the same account-prefix discovery (`/{account}/boards.json` 302s
//! to the sign-in menu without the prefix) and the same Bearer JSON
//! contract (`Accept: application/json` is required — Bearer only honored
//! for JSON).

pub mod format;

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types — narrow, like mirror::fizzy, so we never carry secrets we don't need
// ---------------------------------------------------------------------------

/// A board, as much as the fleet needs to know to create a card.
#[derive(Debug, Clone, Deserialize)]
pub struct Board {
    pub id: String,
    pub name: String,
    /// Present only when the board is published (Fizzy's `_board.json.jbuilder`
    /// guards it). Its presence is the published flag; the value addresses the
    /// internal host, so it is never rendered.
    #[serde(default)]
    pub public_url: Option<String>,
}

/// A card as returned by `GET` and `POST`.
#[derive(Debug, Clone, Deserialize)]
pub struct Card {
    /// Per-account card number — the stable identifier in URLs (`/1/cards/:number`).
    pub number: i64,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub description_html: String,
    pub board: Option<BoardRef>,
    pub creator: Option<User>,
}

/// Minimal board ref embedded in a card.
#[derive(Debug, Clone, Deserialize)]
pub struct BoardRef {
    pub id: String,
    pub name: String,
}

/// Minimal user.
#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub name: String,
}

/// A comment, as returned by `POST /cards/:number/comments.json`.
///
/// Narrow on purpose, like every type here: Fizzy's `_comment.json.jbuilder`
/// also serializes `creator` (which embeds the user partial, and with it an
/// email address), `reactions_url`, and the rich-text `html`. None of that is
/// filtered downstream — it is filtered *here*, by having nowhere to land.
#[derive(Debug, Clone, Deserialize)]
pub struct Comment {
    pub id: String,
    /// Fizzy's canonical URL for the comment.
    pub url: String,
    pub body: CommentBody,
}

/// The rich-text body Fizzy echoes back. Only the plain-text rendering is
/// kept — the `html` sibling is the same content and nothing here renders it.
#[derive(Debug, Clone, Deserialize)]
pub struct CommentBody {
    pub plain_text: String,
}

/// The three card sets disjoint from columns — `GET /boards/:id/columns/:standing.json`.
#[derive(Debug, Clone, Copy)]
pub enum Standing {
    Triage,
    NotNow,
    Closed,
}

impl Standing {
    fn segment(self) -> &'static str {
        match self {
            Standing::Triage => "stream",
            Standing::NotNow => "not_now",
            Standing::Closed => "closed",
        }
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

pub struct Client {
    http: reqwest::Client,
    /// `https://fizzy.intern.deepwa7er.net/1`
    base: String,
    origin: String,
    token: String,
}

impl Client {
    /// `base` is origin (`https://fizzy.intern.deepwa7er.net`), `account` is the
    /// numeric slug (`1`). Without the slug every request 302s to the sign-in
    /// menu rather than failing usefully.
    pub fn new(base: &str, account: &str, token: String, timeout: Duration) -> Result<Self> {
        let origin = base.trim_end_matches('/').to_string();
        if !origin.starts_with("https://") && !origin.starts_with("http://") {
            bail!("fizzy base must be an absolute http(s) URL, got {origin:?}");
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(concat!("fleet-fizzy/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            http,
            base: format!("{origin}/{}", account.trim_matches('/')),
            origin,
            token,
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    async fn get(&self, url: &str) -> Result<reqwest::Response> {
        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if status.is_redirection() {
            bail!("GET {url} redirected ({status}) — token rejected, or the account prefix is wrong");
        }
        if !status.is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("GET {url} failed: {status} — {}", body.chars().take(500).collect::<String>());
        }
        Ok(resp)
    }

    async fn get_paged<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>> {
        let mut url = format!("{}{path}", self.base);
        let mut all = Vec::new();
        for _ in 0..200 {
            let resp = self.get(&url).await?;
            let next = next_page_url(resp.headers());
            let page: Vec<T> = resp
                .json()
                .await
                .with_context(|| format!("parsing the response to GET {url}"))?;
            all.extend(page);
            match next {
                Some(n) if n.starts_with(&self.origin) => url = n,
                Some(n) => bail!("pagination tried to leave {}: {n}", self.origin),
                None => return Ok(all),
            }
        }
        bail!("pagination did not terminate for {path}")
    }

    // --- reads (mirrors mirror::fizzy) ---

    pub async fn boards(&self) -> Result<Vec<Board>> {
        self.get_paged("/boards.json").await
    }

    pub async fn standing_cards(&self, board_id: &str, standing: Standing) -> Result<Vec<Card>> {
        self.get_paged(&format!(
            "/boards/{board_id}/columns/{}.json",
            standing.segment()
        ))
        .await
    }

    /// Fetch one card by its per-account number — the identifier Fizzy puts in
    /// URLs (`/{account}/cards/:number`). The standing/column listings already
    /// carry `description`, so this exists for the case where you have a card
    /// number and not the board it lives on.
    pub async fn card(&self, number: i64) -> Result<Card> {
        let url = format!("{}/cards/{number}.json", self.base);
        let resp = self.get(&url).await?;
        resp.json()
            .await
            .with_context(|| format!("parsing the response to GET {url}"))
    }

    /// Resolve a board by id or by exact name. Name match is case-sensitive
    /// and expects a single hit — ambiguous names error rather than guessing.
    pub async fn resolve_board(&self, id_or_name: &str) -> Result<Board> {
        let boards = self.boards().await?;
        // Prefer id match.
        if let Some(b) = boards.iter().find(|b| b.id == id_or_name) {
            return Ok(b.clone());
        }
        let mut by_name: Vec<_> = boards.iter().filter(|b| b.name == id_or_name).collect();
        match by_name.len() {
            1 => Ok(by_name.remove(0).clone()),
            0 => bail!(
                "no board with id or name {id_or_name:?} — available: {}",
                boards
                    .iter()
                    .map(|b| format!("{} ({})", b.name, b.id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => bail!("multiple boards named {id_or_name:?} — use the board id"),
        }
    }

    // --- writes ---

    /// Send a JSON write request and parse the JSON response.
    ///
    /// `Accept: application/json` is not decoration: Fizzy only honors Bearer
    /// auth when `request.format.json?` (`Authentication#bearer_token_authenticatable_request?`),
    /// so a non-JSON write is not merely unparsed — it is unauthenticated, and
    /// redirects to the sign-in menu instead of failing usefully. That is also
    /// why only endpoints with a JSON representation are reachable from here.
    async fn send_json<B: Serialize, T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        url: &str,
        payload: &B,
    ) -> Result<T> {
        let resp = self
            .http
            .request(method.clone(), url)
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(payload)
            .send()
            .await
            .with_context(|| format!("{method} {url}"))?;

        let status = resp.status();
        if status.is_redirection() {
            bail!(
                "{method} {url} redirected ({status}) — the token was rejected or lacks `write` \
                 permission (read tokens are GET/HEAD only), or the account prefix in {} is wrong",
                self.base
            );
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "{method} {url} failed: {status} — {}",
                body.chars().take(800).collect::<String>()
            );
        }
        resp.json()
            .await
            .with_context(|| format!("parsing the response to {method} {url}"))
    }

    /// Create a published card in `board_id`. Returns the created card
    /// (including its `number` and board URL). `description` is ActionText
    /// rich text — plain markdown is fine; Fizzy renders it.
    pub async fn create_card(
        &self,
        board_id: &str,
        title: &str,
        description: &str,
    ) -> Result<Card> {
        if title.trim().is_empty() {
            bail!("title must not be empty");
        }
        let url = format!("{}/boards/{board_id}/cards.json", self.base);

        #[derive(Serialize)]
        struct Payload<'a> {
            card: CardPayload<'a>,
        }
        #[derive(Serialize)]
        struct CardPayload<'a> {
            title: &'a str,
            description: &'a str,
        }

        self.send_json(
            reqwest::Method::POST,
            &url,
            &Payload {
                card: CardPayload { title, description },
            },
        )
        .await
    }

    /// Post a comment on a card, addressed by its per-account `number`.
    ///
    /// `body` is ActionText rich text, like a card description — Fizzy renders
    /// it, so pass HTML (see `format::markdown_to_html`).
    ///
    /// This is the only supported way to record an outcome against a card from
    /// a token. Fizzy's card *standing* — triage, not-now, closed — is changed
    /// through `Columns::Cards::Drops::*`, whose actions render only
    /// `turbo_stream` and have no JSON representation; since Bearer auth is
    /// honored only for JSON, those routes are unreachable with a token by
    /// design, not by oversight. Closing a card stays a human act in the web UI.
    pub async fn comment_on_card(&self, number: i64, body: &str) -> Result<Comment> {
        if body.trim().is_empty() {
            bail!("comment body must not be empty");
        }
        let url = format!("{}/cards/{number}/comments.json", self.base);

        #[derive(Serialize)]
        struct Payload<'a> {
            comment: CommentPayload<'a>,
        }
        #[derive(Serialize)]
        struct CommentPayload<'a> {
            body: &'a str,
        }

        self.send_json(reqwest::Method::POST, &url, &Payload { comment: CommentPayload { body } })
            .await
            .with_context(|| {
                format!(
                    "commenting on card #{number} (a 403 means the card is not commentable — \
                     `Card::Commentable#commentable?` is `published?`, so drafts reject comments; \
                     closed cards still accept them)"
                )
            })
    }

    /// Update a card's title and/or description, addressed by its per-account
    /// `number`.
    ///
    /// Partial update: only the fields that are `Some` are sent; everything
    /// else is preserved server-side (`CardsController#update` applies only
    /// the provided params). Pass `None` for a field you do not want to change.
    ///
    /// `description` is ActionText rich text, like a card description — Fizzy
    /// renders it, so pass HTML (see `format::markdown_to_html`).
    pub async fn update_card(
        &self,
        number: i64,
        title: Option<&str>,
        description: Option<&str>,
    ) -> Result<Card> {
        if title.is_none() && description.is_none() {
            bail!("nothing to update — pass at least one of title or description");
        }
        let url = format!("{}/cards/{number}.json", self.base);

        #[derive(Serialize)]
        struct Payload<'a> {
            card: UpdateCardPayload<'a>,
        }

        self.send_json(
            reqwest::Method::PUT,
            &url,
            &Payload {
                card: UpdateCardPayload { title, description },
            },
        )
        .await
        .with_context(|| format!("updating card #{number}"))
    }
}

/// The `card` payload for a partial card update — `None` fields are omitted
/// from the request so the server preserves the current value.
#[derive(Serialize)]
struct UpdateCardPayload<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
}

fn next_page_url(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let value = headers.get(reqwest::header::LINK)?.to_str().ok()?;
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
    fn reads_next_link() {
        let h = headers("<https://fizzy.example/1/boards/x/columns/stream.json?page=2>; rel=\"next\"");
        assert_eq!(
            next_page_url(&h).as_deref(),
            Some("https://fizzy.example/1/boards/x/columns/stream.json?page=2")
        );
    }

    fn client(base: &str, account: &str) -> Result<Client> {
        Client::new(base, account, "t".into(), Duration::from_secs(1))
    }

    #[test]
    fn base_carries_the_account_prefix() {
        // Without the prefix every request 302s to the sign-in menu, so the
        // account has to end up in the base exactly once.
        let c = client("https://fizzy.example", "1").unwrap();
        assert_eq!(c.base(), "https://fizzy.example/1");
        assert_eq!(c.origin(), "https://fizzy.example");
    }

    #[test]
    fn trims_slashes_around_origin_and_account() {
        let c = client("https://fizzy.example/", "/1/").unwrap();
        assert_eq!(c.base(), "https://fizzy.example/1");
        assert_eq!(c.origin(), "https://fizzy.example");
    }

    #[test]
    fn rejects_a_base_without_a_scheme() {
        assert!(client("fizzy.example", "1").is_err());
    }

    // The write guards run before the request is built, so these assert real
    // behaviour without touching the network — `fizzy.example` is never resolved.

    #[tokio::test]
    async fn rejects_an_empty_comment_body() {
        let c = client("https://fizzy.example", "1").unwrap();
        assert!(c.comment_on_card(1, "").await.is_err());
        assert!(c.comment_on_card(1, "  \n\t ").await.is_err());
    }

    #[tokio::test]
    async fn rejects_an_empty_card_title() {
        let c = client("https://fizzy.example", "1").unwrap();
        assert!(c.create_card("board", "", "body").await.is_err());
        assert!(c.create_card("board", "   ", "body").await.is_err());
    }

    #[test]
    fn ignores_other_rels() {
        let h = headers("<https://fizzy.example/a>; rel=\"prev\", <https://fizzy.example/b>; rel=\"next\"");
        assert_eq!(next_page_url(&h).as_deref(), Some("https://fizzy.example/b"));
    }

    #[test]
    fn update_payload_omits_unchanged_fields() {
        let payload = UpdateCardPayload {
            title: None,
            description: Some("new body"),
        };
        let v = serde_json::to_value(&payload).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("title"), "{v}");
        assert_eq!(obj["description"], "new body");
    }

    #[test]
    fn update_payload_sends_both_fields_when_provided() {
        let payload = UpdateCardPayload {
            title: Some("new title"),
            description: Some("new body"),
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert_eq!(v["title"], "new title");
        assert_eq!(v["description"], "new body");
    }

    #[tokio::test]
    async fn update_rejects_empty_update_before_any_request() {
        let c = client("https://fizzy.example", "1").unwrap();
        let err = c.update_card(1, None, None).await.unwrap_err();
        assert!(
            err.to_string().contains("nothing to update"),
            "unexpected error: {err:#}"
        );
    }
}
