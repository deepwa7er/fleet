//! Source text → highlight tokens.
//!
//! syntect is used for its *parser* only: scope stacks, not colours. The
//! `highlighting` module and its themes are not compiled in, because the
//! palette is DW-001's five roles and a theme would be a second palette.

use std::sync::OnceLock;

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxSet};

use super::{Token, TokenClass};

/// Loading the default syntax set deserialises a few megabytes, so it happens
/// once, on the first highlighted block, and never on a request path that
/// could not already afford it.
fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// The scope prefixes that map onto each role, most specific first.
///
/// Matching is by prefix on syntect's dotted scope names, so `keyword.control`
/// and `keyword.operator` both land on `Keyword` without enumerating them.
fn class_for(stack: &ScopeStack) -> TokenClass {
    // Walk the stack from the top: the innermost scope is the most specific
    // statement about what this run of text is.
    for scope in stack.as_slice().iter().rev() {
        if let Some(class) = class_for_scope(*scope) {
            return class;
        }
    }
    TokenClass::Plain
}

fn class_for_scope(scope: Scope) -> Option<TokenClass> {
    let name = scope.build_string();
    let head = name.split('.').next().unwrap_or_default();
    match head {
        "keyword" | "storage" => Some(TokenClass::Keyword),
        "comment" => Some(TokenClass::Comment),
        "string" => Some(TokenClass::Str),
        "markup" => match name.split('.').nth(1) {
            Some("deleted") => Some(TokenClass::Deleted),
            Some("inserted") => Some(TokenClass::Inserted),
            _ => None,
        },
        _ => None,
    }
}

/// Tokenise `source` as `lang`.
///
/// An unknown or absent language yields one `Plain` token — honest metadata
/// and no guessing, which is the same choice the Rails renderer made: an
/// unrecognised fence is labelled but not highlighted.
pub fn tokenize(lang: Option<&str>, source: &str) -> Vec<Token> {
    let Some(syntax) = lang.and_then(|lang| {
        let syntaxes = syntaxes();
        syntaxes
            .find_syntax_by_token(lang)
            .or_else(|| syntaxes.find_syntax_by_extension(lang))
    }) else {
        return plain(source);
    };

    let syntaxes = syntaxes();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut tokens: Vec<Token> = Vec::new();

    for line in source.split_inclusive('\n') {
        // A malformed or pathological line is not worth failing a whole
        // transcript over; the block degrades to plain text from here.
        let Ok(ops) = state.parse_line(line, syntaxes) else {
            push(&mut tokens, TokenClass::Plain, line);
            continue;
        };
        let mut cursor = 0;
        for (index, op) in ops {
            if index > cursor {
                push(&mut tokens, class_for(&stack), &line[cursor..index]);
                cursor = index;
            }
            // An op that does not apply cleanly means the scope stack and the
            // syntax have diverged; keeping the old stack yields slightly
            // wrong colours, which is strictly better than dropping the text.
            let _ = stack.apply(&op);
        }
        if cursor < line.len() {
            push(&mut tokens, class_for(&stack), &line[cursor..]);
        }
    }
    tokens
}

fn plain(source: &str) -> Vec<Token> {
    if source.is_empty() {
        return Vec::new();
    }
    vec![Token { class: TokenClass::Plain, text: source.to_owned() }]
}

/// Append text, merging into the previous token when the class is unchanged.
///
/// syntect emits an op at every scope boundary, which for ordinary code means
/// many adjacent `Plain` runs. Merging is what keeps the wire proportional to
/// the *interesting* structure rather than to the parser's chattiness.
fn push(tokens: &mut Vec<Token>, class: TokenClass, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = tokens.last_mut()
        && last.class == class
    {
        last.text.push_str(text);
        return;
    }
    tokens.push(Token { class, text: text.to_owned() });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(tokens: &[Token]) -> Vec<TokenClass> {
        tokens.iter().map(|t| t.class).collect()
    }

    fn text_of(tokens: &[Token], class: TokenClass) -> String {
        tokens.iter().filter(|t| t.class == class).map(|t| t.text.as_str()).collect()
    }

    #[test]
    fn tokens_reassemble_into_the_original_source() {
        // The single property that must never break: highlighting may not
        // lose, duplicate, or reorder a byte of the program.
        let source = "fn main() {\n    // hi\n    let x = \"s\";\n}\n";
        let joined: String =
            tokenize(Some("rust"), source).iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, source);
    }

    #[test]
    fn rust_keywords_strings_and_comments_land_on_their_roles() {
        let tokens = tokenize(Some("rust"), "fn f() {\n// note\nlet s = \"hello\";\n}\n");
        assert!(text_of(&tokens, TokenClass::Keyword).contains("fn"));
        assert!(text_of(&tokens, TokenClass::Comment).contains("note"));
        assert!(text_of(&tokens, TokenClass::Str).contains("hello"));
    }

    #[test]
    fn an_unknown_language_is_one_plain_run_rather_than_a_guess() {
        let tokens = tokenize(Some("klingon"), "fn main() {}");
        assert_eq!(classes(&tokens), [TokenClass::Plain]);
        assert_eq!(tokens[0].text, "fn main() {}");
    }

    #[test]
    fn an_absent_language_is_not_highlighted() {
        let tokens = tokenize(None, "fn main() {}");
        assert_eq!(classes(&tokens), [TokenClass::Plain]);
    }

    #[test]
    fn adjacent_runs_of_the_same_role_are_merged() {
        let tokens = tokenize(Some("rust"), "let a = 1;\n");
        // Without merging, syntect's per-boundary ops would produce a long run
        // of single-character Plain tokens.
        for pair in tokens.windows(2) {
            assert_ne!(pair[0].class, pair[1].class, "adjacent tokens share a class");
        }
    }

    #[test]
    fn empty_source_produces_no_tokens() {
        assert!(tokenize(Some("rust"), "").is_empty());
        assert!(tokenize(None, "").is_empty());
    }

    #[test]
    fn a_language_can_be_named_by_its_extension() {
        let tokens = tokenize(Some("rs"), "fn f() {}");
        assert!(tokens.iter().any(|t| t.class == TokenClass::Keyword));
    }

    #[test]
    fn a_diff_uses_the_inserted_and_deleted_roles() {
        let tokens = tokenize(Some("diff"), "--- a\n+++ b\n-gone\n+added\n");
        assert!(text_of(&tokens, TokenClass::Deleted).contains("gone"));
        assert!(text_of(&tokens, TokenClass::Inserted).contains("added"));
    }
}
