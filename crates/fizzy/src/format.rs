//! Card body formatting — the readability boundary.
//!
//! Agents POST free-form markdown as `description`. Without structure the
//! cards collapse into walls of text (missing blank lines around headings,
//! no `Why / Evidence / Options` scaffold, shell-escaped one-liners).
//! This module is the single place that normalises and validates bodies so
//! every `create` path — `fizzy draft` + `fizzy create --body-file` — renders
//! the same in Fizzy's Trix/ActionText view.

use anyhow::{bail, Result};
use pulldown_cmark::{html, Options, Parser};

/// Template for triage cards. Matches the `Why / Evidence / Options /
/// Provenance` structure described in `.agents/skills/fizzy/SKILL.md`.
pub fn card_template() -> String {
    r#"## Why

<!-- 1-3 sentences: who is affected and why it matters. -->

## Evidence

- `path/to/file.rs:123` — what you observed
- `other/file:45` — second piece of evidence

## Options

1. Preferred approach — why it is preferred
2. Alternative — trade-offs

---
Provenance: session <id>, commit <hash>
"#
    .to_string()
}

/// Draft path slug from a title: `fleet: blog — backup gap` → `fleet-blog-backup-gap`.
pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in title.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            // collapse any run of non-alnum into a single dash
            slug.push('-');
            prev_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "card".to_string()
    } else if slug.len() > 48 {
        slug[..48].trim_matches('-').to_string()
    } else {
        slug
    }
}

/// Normalise markdown so it renders reliably in ActionText/Trix.
///
/// Guarantees:
/// - `\r\n` → `\n`, trailing whitespace stripped
/// - no leading/trailing blank lines, at most one consecutive blank line
/// - blank line before and after every ATX heading (`# …`, `## …`)
/// - blank line after a heading before the next content block
/// - collapses `---`/`***` horizontal rules onto their own line with surrounding blanks
/// - preserves fenced code blocks verbatim (except trailing ws)
/// - ends with a single `\n` if non-empty
pub fn normalize_body(input: &str) -> String {
    // 1. Normalise line endings and rtrim each line.
    let normalised = input.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = normalised
        .lines()
        .map(|l| l.trim_end().to_string())
        .collect();

    // Trim leading/trailing blank lines.
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return String::new();
    }

    // 2. Collapse runs of blank lines to a single blank (outside fences we will re-handle,
    // but collapse first to simplify fence tracking).
    let mut collapsed: Vec<String> = Vec::new();
    let mut in_fence = false;
    let mut prev_empty = false;
    for line in lines {
        let fence = line.trim_start().starts_with("```");
        if fence {
            in_fence = !in_fence;
            collapsed.push(line);
            prev_empty = false;
            continue;
        }
        if in_fence {
            collapsed.push(line);
            prev_empty = false;
            continue;
        }
        let is_empty = line.trim().is_empty();
        if is_empty {
            if !prev_empty {
                collapsed.push(String::new());
            }
            prev_empty = true;
        } else {
            collapsed.push(line);
            prev_empty = false;
        }
    }

    // 3. Ensure heading and thematic-break spacing, and heading blank-after.
    let mut spaced: Vec<String> = Vec::new();
    in_fence = false;
    let mut prev_was_heading = false;
    let mut prev_was_thematic = false;

    for line in collapsed {
        let fence = line.trim_start().starts_with("```");
        if fence {
            // ensure blank before fence (opening) if previous block not already blank
            if !spaced.is_empty()
                && !spaced.last().unwrap().trim().is_empty()
                && !in_fence
            {
                spaced.push(String::new());
            }
            spaced.push(line);
            in_fence = !in_fence;
            prev_was_heading = false;
            prev_was_thematic = false;
            continue;
        }
        if in_fence {
            spaced.push(line);
            continue;
        }

        let trimmed = line.trim();
        let is_empty = trimmed.is_empty();
        if is_empty {
            spaced.push(line);
            prev_was_heading = false;
            prev_was_thematic = false;
            continue;
        }

        let is_heading = trimmed.starts_with('#');
        let is_thematic = trimmed == "---" || trimmed == "***" || trimmed == "___";

        if is_heading {
            // blank before heading (unless first block)
            if !spaced.is_empty() && !spaced.last().unwrap().trim().is_empty() {
                spaced.push(String::new());
            }
            // normalise heading: trim and ensure single space after hashes
            spaced.push(normalise_heading(&line));
            prev_was_heading = true;
            prev_was_thematic = false;
            continue;
        }

        if is_thematic {
            if !spaced.is_empty() && !spaced.last().unwrap().trim().is_empty() {
                spaced.push(String::new());
            }
            spaced.push("---".to_string());
            prev_was_heading = false;
            prev_was_thematic = true;
            continue;
        }

        // regular content line — ensure blank after heading/thematic/fence
        let needs_blank_after_block = prev_was_heading
            || prev_was_thematic
            || spaced
                .last()
                .is_some_and(|l| l.trim_start().starts_with("```"));
        if needs_blank_after_block && !spaced.last().unwrap().trim().is_empty() {
            let last = spaced.pop().unwrap();
            spaced.push(last);
            spaced.push(String::new());
        }

        // Light list normalisation: ensure `-text` → `- text`, `*text` → `* text`
        let normalised_line = normalise_list_marker(&line);
        spaced.push(normalised_line);
        prev_was_heading = false;
        prev_was_thematic = false;
    }

    // 4. Final collapse: heading spacing may have introduced double blanks; re-collapse outside fences.
    let mut final_lines: Vec<String> = Vec::new();
    in_fence = false;
    prev_empty = false;
    for line in spaced {
        let fence = line.trim_start().starts_with("```");
        if fence {
            in_fence = !in_fence;
            final_lines.push(line);
            prev_empty = false;
            continue;
        }
        if in_fence {
            final_lines.push(line);
            prev_empty = false;
            continue;
        }
        let is_empty = line.trim().is_empty();
        if is_empty {
            if !prev_empty {
                final_lines.push(String::new());
            }
            prev_empty = true;
        } else {
            final_lines.push(line);
            prev_empty = false;
        }
    }

    // Remove trailing blank again after spacing, ensure single trailing newline.
    while final_lines
        .last()
        .is_some_and(|l| l.trim().is_empty())
    {
        final_lines.pop();
    }

    if final_lines.is_empty() {
        String::new()
    } else {
        final_lines.join("\n") + "\n"
    }
}

fn normalise_heading(line: &str) -> String {
    let trimmed = line.trim();
    // count leading #
    let hash_len = trimmed.chars().take_while(|&c| c == '#').count();
    let rest = trimmed[hash_len..].trim();
    if rest.is_empty() {
        trimmed.to_string()
    } else {
        format!("{} {}", "#".repeat(hash_len), rest)
    }
}

fn normalise_list_marker(line: &str) -> String {
    let trimmed_start = line.trim_start();
    let indent_len = line.len() - trimmed_start.len();
    let indent = &line[..indent_len];

    // unordered: -, *, +
    if let Some(rest) = trimmed_start.strip_prefix("-") {
        if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
            return format!("{indent}- {}", rest.trim_start());
        }
    }
    if let Some(rest) = trimmed_start.strip_prefix('*') {
        // avoid `**bold**` — must be `* ` with space
        if rest.starts_with(' ') || rest.is_empty() {
            return line.to_string();
        }
        // distinguish emphasis/bold from list: `*text` at line start with no space is likely list if not followed by another `*`
        if !rest.starts_with('*') && !rest.is_empty() {
            return format!("{indent}* {}", rest.trim_start());
        }
    }
    if let Some(rest) = trimmed_start.strip_prefix('+') {
        if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
            return format!("{indent}+ {}", rest.trim_start());
        }
    }
    // ordered: `1.`, `2.` etc — ensure space after dot
    if let Some(dot) = trimmed_start.find('.') {
        let (num, after) = trimmed_start.split_at(dot);
        if num.chars().all(|c| c.is_ascii_digit()) && !num.is_empty() {
            let rest = &after[1..]; // skip dot
            if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
                return format!("{indent}{}. {}", num, rest.trim_start());
            }
        }
    }
    line.to_string()
}

/// Validate title. Returns `Ok` if acceptable, otherwise a human-readable error.
pub fn validate_title(title: &str) -> Result<()> {
    let t = title.trim();
    if t.is_empty() {
        bail!("title must not be empty — use \"fleet: <area> — <what>\" for triage cards");
    }
    if t.len() > 120 {
        bail!("title is {} chars — keep it under 120 (current: {t:?})", t.len());
    }
    if t.to_lowercase() == "untitled" {
        bail!("title must not be \"Untitled\"");
    }
    Ok(())
}

/// Validate body, returning warnings. Fleet-prefixed titles are held to a
/// stricter scaffold (`## Why` + `## Evidence`); other cards only need to be
/// non-empty to avoid blocking idea cards like “Serverless?”.
pub fn validate_body(body: &str, title: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let trimmed = body.trim();
    if trimmed.is_empty() {
        warnings.push("body is empty — add Why / Evidence".to_string());
        return warnings;
    }

    let is_fleet = title.trim().to_lowercase().starts_with("fleet:");
    let has_why = contains_heading(trimmed, "why");
    let has_evidence = contains_heading(trimmed, "evidence");

    if is_fleet {
        if !has_why {
            warnings.push("missing `## Why` — explain who is affected and why".to_string());
        }
        if !has_evidence {
            warnings.push(
                "missing `## Evidence` — cite file:line or observable output".to_string(),
            );
        }
        // Provenance is helpful but not required for idea cards; only warn for fleet cards without it.
        if !trimmed.to_lowercase().contains("provenance") {
            warnings.push(
                "missing `Provenance` — add `Provenance: session …, commit …`".to_string(),
            );
        }
        if is_fleet && title.trim().contains(':') && !title.contains('—') && !title.contains("--") {
            warnings.push(
                "fleet title should use an em dash: \"fleet: <area> — <what>\"".to_string(),
            );
        }
    } else {
        // For non-fleet cards, only warn if body is very short and unstructured — likely accidental one-liner.
        if trimmed.len() < 20 && !has_why && !has_evidence {
            // not a warning, just a hint — but we keep quiet to avoid nagging idea cards.
        }
    }

    warnings
}

fn contains_heading(body: &str, name: &str) -> bool {
    let needle = name.to_lowercase();
    body.lines().any(|l| {
        let t = l.trim().to_lowercase();
        // match `## why`, `### why`, `## why —` etc.
        if t.starts_with('#') {
            let without_hash = t.trim_start_matches('#').trim();
            without_hash == needle || without_hash.starts_with(&format!("{needle} "))
        } else {
            false
        }
    })
}

/// Convert normalised markdown to HTML for Fizzy's ActionText/Trix.
///
/// Markdown headings (`## Why`), lists (`- item`, `1. item`), code fences,
/// and `---` rules become `<h2>`, `<ul>/<ol><li>`, `<pre><code>`, `<hr>` etc,
/// which is what `description_html` renders. Plain text stays `<p>`.
/// Empty input stays empty (Fizzy treats empty as no body).
pub fn markdown_to_html(markdown: &str) -> String {
    if markdown.trim().is_empty() {
        return String::new();
    }
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(markdown, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_to_html_renders_headings_and_lists() {
        let md = "## Why\n\ncontent\n\n## Evidence\n\n- a\n- b\n";
        let html = markdown_to_html(md);
        assert!(html.contains("<h2>Why</h2>"), "{html}");
        assert!(html.contains("<h2>Evidence</h2>"), "{html}");
        assert!(html.contains("<ul>"), "{html}");
        assert!(html.contains("<li>a</li>"), "{html}");
    }

    #[test]
    fn markdown_to_html_empty() {
        assert_eq!(markdown_to_html(""), "");
        assert_eq!(markdown_to_html("   \n"), "");
    }

    #[test]
    fn markdown_to_html_code_fence() {
        let md = "```rust\nfn x() {}\n```\n";
        let html = markdown_to_html(md);
        assert!(html.contains("<pre><code"), "{html}");
        assert!(html.contains("fn x()"), "{html}");
    }

    #[test]
    fn template_has_required_sections() {
        let t = card_template();
        assert!(contains_heading(&t, "why"));
        assert!(contains_heading(&t, "evidence"));
        assert!(t.contains("Provenance"));
    }

    #[test]
    fn normalize_adds_blank_around_headings() {
        let raw = "## Why\ntesting\n## Evidence\n- file.rs:123";
        let out = normalize_body(raw);
        assert_eq!(
            out,
            "## Why\n\ntesting\n\n## Evidence\n\n- file.rs:123\n"
        );
    }

    #[test]
    fn normalize_collapses_multiple_blanks_and_trims() {
        let raw = "\n\n## Why  \n\n\ncontent   \n\n\n## Evidence\n\n- a\n- b  \n\n\n";
        let out = normalize_body(raw);
        assert_eq!(out, "## Why\n\ncontent\n\n## Evidence\n\n- a\n- b\n");
    }

    #[test]
    fn normalize_heading_spacing() {
        let raw = "#Title\ncontent";
        assert_eq!(normalize_body(raw), "# Title\n\ncontent\n");
    }

    #[test]
    fn normalize_list_marker() {
        assert_eq!(normalize_body("-item\n"), "- item\n");
        assert_eq!(normalize_body("*item\n"), "* item\n");
        assert_eq!(normalize_body("1.item\n"), "1. item\n");
        // already correct stays
        assert_eq!(normalize_body("- item\n"), "- item\n");
        assert_eq!(normalize_body("* item\n"), "* item\n");
    }

    #[test]
    fn normalize_thematic_break() {
        let raw = "content\n---\nProvenance: x";
        assert_eq!(normalize_body(raw), "content\n\n---\n\nProvenance: x\n");
    }

    #[test]
    fn normalize_preserves_fences() {
        let raw = "## Why\n```rust\n# not a heading\n```\ncontent";
        let out = normalize_body(raw);
        assert_eq!(out, "## Why\n\n```rust\n# not a heading\n```\n\ncontent\n");
    }

    #[test]
    fn normalize_empty() {
        assert_eq!(normalize_body(""), "");
        assert_eq!(normalize_body("   \n\n  "), "");
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("fleet: blog — backup gap"), "fleet-blog-backup-gap");
        assert_eq!(slugify("  Hello World!  "), "hello-world");
        assert_eq!(slugify(""), "card");
    }

    #[test]
    fn validate_fleet_missing_sections() {
        let w = validate_body("just a short note", "fleet: something — thing");
        assert!(w.iter().any(|m| m.contains("Why")));
        assert!(w.iter().any(|m| m.contains("Evidence")));
    }

    #[test]
    fn validate_idea_card_no_warnings() {
        let w = validate_body("we need a web ui for sonar", "sonar - web ui");
        assert!(w.is_empty());
    }

    #[test]
    fn validate_title_rejects_empty() {
        assert!(validate_title("").is_err());
        assert!(validate_title("   ").is_err());
        assert!(validate_title("Untitled").is_err());
    }

    #[test]
    fn contains_heading_case_insensitive() {
        assert!(contains_heading("## Why", "why"));
        assert!(contains_heading("### Evidence — foo", "evidence"));
        assert!(!contains_heading("why is this", "why"));
    }
}
