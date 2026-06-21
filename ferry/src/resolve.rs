use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

use crate::config::{Config, QUERY_PLACEHOLDER};

#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Redirect the browser to this URL.
    Redirect(String),
    /// Capture a note: POST `text` to the notes app's `api`, then show a
    /// confirmation page that links `open`. (The keyword with no text resolves
    /// to `Redirect(open)` instead — there's nothing to capture.)
    Capture { api: String, text: String, open: String },
    /// Show the page listing all configured commands.
    ListPage,
}

/// Turn raw address-bar input into a destination.
///
/// The first whitespace-separated word names a command; the rest is its
/// argument. Rules, in order:
/// - empty input, or the bare word `list` (unless shadowed by config): the list page
/// - explicit command match takes precedence over the built-ins below:
///   - parameterized command (template contains `{query}` or `{1}`..`{9}`):
///     substitute the (encoded) argument(s)
///   - command with no placeholders and no argument: redirect to it directly
/// - the `:port` shorthand (unless shadowed by an explicit command of that name):
///   `:3000` or `:3000/path` jumps to `http://localhost:3000[/path]`
/// - anything else (unknown command, or argument given to a command that
///   takes none): treat the whole input as a fallback search
pub fn resolve(config: &Config, input: &str) -> Resolution {
    let input = input.trim();
    if input.is_empty() {
        return Resolution::ListPage;
    }

    let (name, args) = match input.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim()),
        None => (input, ""),
    };

    if let Some(template) = config.commands.get(name) {
        if is_parameterized(template) {
            return Resolution::Redirect(fill(template, args));
        }
        if args.is_empty() {
            return Resolution::Redirect(template.clone());
        }
        return Resolution::Redirect(fill(&config.fallback, input));
    }

    // The note-capture keyword (after explicit commands, so it can be shadowed):
    // with text it captures, bare it just opens the notes app.
    if let Some(capture) = config.capture.as_ref().filter(|c| c.keyword == name) {
        if args.is_empty() {
            return Resolution::Redirect(capture.open.clone());
        }
        return Resolution::Capture {
            api: capture.api.clone(),
            text: args.to_string(),
            open: capture.open.clone(),
        };
    }

    if let Some(url) = localhost_redirect(name) {
        return Resolution::Redirect(url);
    }

    if input == "list" {
        return Resolution::ListPage;
    }

    Resolution::Redirect(fill(&config.fallback, input))
}

/// Built-in `:port` shorthand: `:3000` → `http://localhost:3000`. An optional
/// path/query/fragment suffix is carried through verbatim (`:3000/admin` →
/// `http://localhost:3000/admin`). Returns `None` — so the caller falls through
/// to a normal search — unless the token is `:` followed by a valid port (a
/// 1..=65535 run of digits) and any suffix begins with `/`, `?`, or `#`.
fn localhost_redirect(token: &str) -> Option<String> {
    let rest = token.strip_prefix(':')?;
    let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let (digits, suffix) = rest.split_at(digits_end);
    // A valid port (rejects empty, 0, and anything past u16::MAX like `:99999`).
    let port: u16 = digits.parse().ok().filter(|&p| p != 0)?;
    // Reject a token that merely starts with digits, e.g. `:3000abc` or `::1`.
    if !suffix.is_empty() && !suffix.starts_with(['/', '?', '#']) {
        return None;
    }
    Some(format!("http://localhost:{port}{suffix}"))
}

/// Highest positional placeholder supported: `{1}`..`{9}`.
const MAX_POSITIONAL: usize = 9;

/// Whether a template has any substitution placeholder — `{query}` (the whole
/// argument string) or a positional `{1}`..`{9}` (one whitespace-separated arg).
fn is_parameterized(template: &str) -> bool {
    template.contains(QUERY_PLACEHOLDER)
        || (1..=MAX_POSITIONAL).any(|i| template.contains(&format!("{{{i}}}")))
}

/// Substitute placeholders with percent-encoded arguments. `{query}` becomes
/// the whole argument string; `{N}` becomes the Nth whitespace-separated
/// argument, or empty if there are fewer than N.
fn fill(template: &str, args: &str) -> String {
    let encode = |value: &str| utf8_percent_encode(value, NON_ALPHANUMERIC).to_string();
    let mut result = template.replace(QUERY_PLACEHOLDER, &encode(args));
    let parts: Vec<&str> = args.split_whitespace().collect();
    for i in 1..=MAX_POSITIONAL {
        let value = parts.get(i - 1).map(|part| encode(part)).unwrap_or_default();
        result = result.replace(&format!("{{{i}}}"), &value);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        toml::from_str(
            r#"
            fallback = "https://search.example/?q={query}"
            [commands]
            mail = "https://mail.example/"
            gh = "https://github.com/search?q={query}"
            list = "https://lists.example/"
            svc = "https://dash.example/services/{1}?action={2}"
            "#,
        )
        .unwrap()
    }

    fn redirect(url: &str) -> Resolution {
        Resolution::Redirect(url.to_string())
    }

    fn config_with_capture() -> Config {
        toml::from_str(
            r#"
            fallback = "https://search.example/?q={query}"
            [commands]
            mail = "https://mail.example/"
            [capture]
            keyword = "lg"
            api = "http://127.0.0.1:8092/api/thoughts"
            open = "https://notes.example/"
            "#,
        )
        .unwrap()
    }

    #[test]
    fn static_command_redirects() {
        assert_eq!(resolve(&config(), "mail"), redirect("https://mail.example/"));
    }

    #[test]
    fn parameterized_command_encodes_argument() {
        assert_eq!(
            resolve(&config(), "gh hyper tls"),
            redirect("https://github.com/search?q=hyper%20tls"),
        );
    }

    #[test]
    fn parameterized_command_accepts_empty_argument() {
        assert_eq!(resolve(&config(), "gh"), redirect("https://github.com/search?q="));
    }

    #[test]
    fn unknown_input_falls_back_to_search() {
        assert_eq!(
            resolve(&config(), "what is a monad"),
            redirect("https://search.example/?q=what%20is%20a%20monad"),
        );
    }

    #[test]
    fn argument_to_static_command_falls_back_to_search() {
        assert_eq!(
            resolve(&config(), "mail compose"),
            redirect("https://search.example/?q=mail%20compose"),
        );
    }

    #[test]
    fn empty_input_shows_list_page() {
        assert_eq!(resolve(&config(), "  "), Resolution::ListPage);
    }

    #[test]
    fn positional_args_substituted_independently() {
        assert_eq!(
            resolve(&config(), "svc navidrome restart"),
            redirect("https://dash.example/services/navidrome?action=restart"),
        );
    }

    #[test]
    fn missing_positional_arg_is_empty() {
        // `svc navidrome` with no action → `{2}` resolves to empty.
        assert_eq!(
            resolve(&config(), "svc navidrome"),
            redirect("https://dash.example/services/navidrome?action="),
        );
    }

    #[test]
    fn positional_args_are_encoded() {
        assert_eq!(
            resolve(&config(), "svc a/b c d"),
            // only {1} and {2} are used; the third arg is ignored
            redirect("https://dash.example/services/a%2Fb?action=c"),
        );
    }

    #[test]
    fn colon_port_jumps_to_localhost() {
        assert_eq!(resolve(&config(), ":3000"), redirect("http://localhost:3000"));
    }

    #[test]
    fn colon_port_carries_path_query_and_fragment() {
        assert_eq!(
            resolve(&config(), ":3000/admin"),
            redirect("http://localhost:3000/admin"),
        );
        assert_eq!(
            resolve(&config(), ":8080/search?q=hi#top"),
            redirect("http://localhost:8080/search?q=hi#top"),
        );
    }

    #[test]
    fn colon_port_rejects_non_port_input() {
        // Not a port → ordinary fallback search, colon and all.
        for input in [":", ":abc", ":99999", ":0", "::1", ":3000abc"] {
            assert_eq!(
                resolve(&config(), input),
                redirect(&fill("https://search.example/?q={query}", input)),
                "{input:?} should fall through to search",
            );
        }
    }

    #[test]
    fn colon_port_can_be_shadowed_by_an_explicit_command() {
        let mut config = config();
        config
            .commands
            .insert(":3000".to_string(), "https://override.example/".to_string());
        assert_eq!(resolve(&config, ":3000"), redirect("https://override.example/"));
    }

    #[test]
    fn bare_list_shows_list_page_unless_configured() {
        let mut config = config();
        assert_eq!(resolve(&config, "list"), redirect("https://lists.example/"));
        config.commands.remove("list");
        assert_eq!(resolve(&config, "list"), Resolution::ListPage);
    }

    #[test]
    fn capture_keyword_with_text_captures() {
        assert_eq!(
            resolve(&config_with_capture(), "lg buy oat milk"),
            Resolution::Capture {
                api: "http://127.0.0.1:8092/api/thoughts".to_string(),
                text: "buy oat milk".to_string(),
                open: "https://notes.example/".to_string(),
            },
        );
    }

    #[test]
    fn bare_capture_keyword_opens_notes_app() {
        // Nothing to capture → just open the notes app.
        assert_eq!(resolve(&config_with_capture(), "lg"), redirect("https://notes.example/"));
    }

    #[test]
    fn capture_text_is_raw_not_url_encoded() {
        // The text becomes a JSON body, so it must NOT be percent-encoded here.
        match resolve(&config_with_capture(), "lg a & b?") {
            Resolution::Capture { text, .. } => assert_eq!(text, "a & b?"),
            other => panic!("expected capture, got {other:?}"),
        }
    }

    #[test]
    fn explicit_command_shadows_capture_keyword() {
        let mut config = config_with_capture();
        config
            .commands
            .insert("lg".to_string(), "https://override.example/?q={query}".to_string());
        // The explicit command wins; capture never triggers.
        assert_eq!(resolve(&config, "lg note"), redirect("https://override.example/?q=note"));
    }
}
