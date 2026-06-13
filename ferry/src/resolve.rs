use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

use crate::config::{Config, QUERY_PLACEHOLDER};

#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Redirect the browser to this URL.
    Redirect(String),
    /// Show the page listing all configured commands.
    ListPage,
}

/// Turn raw address-bar input into a destination.
///
/// The first whitespace-separated word names a command; the rest is its
/// argument. Rules, in order:
/// - empty input, or the bare word `list` (unless shadowed by config): the list page
/// - parameterized command (template contains `{query}` or `{1}`..`{9}`):
///   substitute the (encoded) argument(s)
/// - command with no placeholders and no argument: redirect to it directly
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

    if input == "list" {
        return Resolution::ListPage;
    }

    Resolution::Redirect(fill(&config.fallback, input))
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
    fn bare_list_shows_list_page_unless_configured() {
        let mut config = config();
        assert_eq!(resolve(&config, "list"), redirect("https://lists.example/"));
        config.commands.remove("list");
        assert_eq!(resolve(&config, "list"), Resolution::ListPage);
    }
}
