use crate::config::Config;

#[derive(Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The completed input the browser offers (a command name).
    pub completion: String,
    /// Shown alongside the completion by browsers that support descriptions.
    pub description: String,
}

/// Command-name completions for a partial address-bar input.
///
/// Only the command word (the first token) can be completed; once the input
/// contains whitespace the user is typing arguments and there is nothing
/// useful to suggest. Matching is a case-insensitive prefix match. An empty
/// input matches every command, which lets the browser show the full list.
pub fn suggest(config: &Config, input: &str) -> Vec<Suggestion> {
    let prefix = input.trim_start();
    if prefix.contains(char::is_whitespace) {
        return Vec::new();
    }
    let prefix = prefix.to_lowercase();

    let mut suggestions: Vec<Suggestion> = config
        .commands
        .iter()
        .filter(|(name, _)| name.to_lowercase().starts_with(&prefix))
        .map(|(name, url)| Suggestion {
            completion: name.clone(),
            description: url.clone(),
        })
        .collect();

    // Action keywords are first-class triggers, so offer them too — unless a
    // command of the same name already shadows the action (and is listed above).
    for action in &config.action {
        if action.keyword.to_lowercase().starts_with(&prefix)
            && !config.commands.contains_key(&action.keyword)
        {
            suggestions.push(Suggestion {
                completion: action.keyword.clone(),
                description: format!("POST \u{2192} {}", action.url),
            });
        }
    }

    if !config.commands.contains_key("list") && "list".starts_with(&prefix) {
        suggestions.push(Suggestion {
            completion: "list".to_string(),
            description: "show all ferry commands".to_string(),
        });
    }

    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::from_toml(
            r#"
            fallback = "https://search.example/?q={query}"
            [commands]
            gh = "https://github.com/search?q={query}"
            mail = "https://mail.example/"
            maps = "https://maps.example/"

            [[action]]
            keyword = "tug"
            url = "https://tugboat.example/deploy/{1}"
            "#,
        )
        .unwrap()
    }

    fn completions(input: &str) -> Vec<String> {
        suggest(&config(), input).into_iter().map(|s| s.completion).collect()
    }

    #[test]
    fn prefix_matches_command_names() {
        assert_eq!(completions("ma"), ["mail", "maps"]);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(completions("MA"), ["mail", "maps"]);
    }

    #[test]
    fn empty_input_lists_everything() {
        // Commands (sorted), then action keywords, then the built-in `list`.
        assert_eq!(completions(""), ["gh", "mail", "maps", "tug", "list"]);
    }

    #[test]
    fn action_keyword_is_suggested() {
        assert_eq!(completions("tu"), ["tug"]);
        let described: Vec<_> =
            suggest(&config(), "tug").into_iter().map(|s| s.description).collect();
        assert_eq!(described, ["POST \u{2192} https://tugboat.example/deploy/{1}"]);
    }

    #[test]
    fn command_shadows_action_in_suggestions() {
        let mut config = config();
        config.commands.insert("tug".into(), "https://override.example/".into());
        // Only the command appears, not a duplicate action entry.
        let described: Vec<_> =
            suggest(&config, "tug").into_iter().map(|s| s.description).collect();
        assert_eq!(described, ["https://override.example/"]);
    }

    #[test]
    fn input_with_arguments_gets_no_suggestions() {
        assert!(completions("gh axum").is_empty());
        assert!(completions("gh ").is_empty());
    }

    #[test]
    fn builtin_list_is_suggested_unless_shadowed() {
        assert_eq!(completions("li"), ["list"]);
        let mut config = config();
        config.commands.insert("list".into(), "https://lists.example/".into());
        let from_config: Vec<_> =
            suggest(&config, "li").into_iter().map(|s| s.description).collect();
        assert_eq!(from_config, ["https://lists.example/"]);
    }
}
