//! Incremental markdown rendering for the model's streamed answer text.
//!
//! Deltas arrive token-by-token, but code fences and syntax highlighting are
//! line-oriented, so deltas are buffered until a complete line arrives and
//! then rendered:
//!
//!   - fenced code blocks (``` or ~~~) are highlighted with syntect, whose
//!     HighlightLines is itself incremental (one line at a time), so it
//!     composes with streaming; the fence lines themselves render as blank
//!     lines, keeping code visually separated from prose
//!   - inline `code` spans in prose are colored
//!   - the last partial line is held until its newline arrives (or the
//!     stream ends, when finish() flushes it)
//!
//! Every rendered line ends with an ANSI reset, so terminal state is always
//! clean at line boundaries — aborting mid-stream (Ctrl-C, interjection)
//! never leaves the terminal colored. When stdout is not a terminal the
//! renderer passes bytes through untouched, so piped output stays clean
//! markdown.

use std::io::{IsTerminal, Write};
use std::sync::LazyLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

// Lazily loaded on the first fenced code block (or inline span) rather than
// at startup: the nonewlines variant matches our stripped line endings.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_nonewlines);
static THEME: LazyLock<Theme> =
    LazyLock::new(|| ThemeSet::load_defaults().themes["base16-ocean.dark"].clone());

const INLINE_CODE: &str = "\x1b[36m"; // cyan
const RESET: &str = "\x1b[0m";

pub struct StreamRenderer {
    enabled: bool,
    pending: String,
    fence: Option<char>, // '`' or '~' while inside a code block
    hl: Option<HighlightLines<'static>>,
}

impl Default for StreamRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamRenderer {
    pub fn new() -> Self {
        StreamRenderer {
            enabled: std::io::stdout().is_terminal(),
            pending: String::new(),
            fence: None,
            hl: None,
        }
    }

    /// Feed a content delta and print whatever complete lines it yields.
    pub fn push(&mut self, delta: &str) {
        let rendered = self.push_str(delta);
        if !rendered.is_empty() {
            print!("{rendered}");
            std::io::stdout().flush().ok();
        }
    }

    /// Stream finished: print any partial line still in the buffer.
    pub fn finish(&mut self) {
        let rendered = self.finish_str();
        if !rendered.is_empty() {
            print!("{rendered}");
            std::io::stdout().flush().ok();
        }
    }

    /// Testable core of push(): returns the text to print now (may be empty).
    fn push_str(&mut self, delta: &str) -> String {
        if !self.enabled {
            return delta.to_string(); // piped: raw passthrough, no buffering
        }
        self.pending.push_str(delta);
        let mut out = String::new();
        // A '\n' is one byte and UTF-8 continuation bytes are >= 0x80, so
        // splitting on it never breaks a codepoint.
        while let Some(pos) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=pos).collect();
            out.push_str(&self.render_line(line.trim_end_matches('\n')));
            out.push('\n');
        }
        out
    }

    /// Testable core of finish(): renders the buffered partial line, if any.
    fn finish_str(&mut self) -> String {
        if !self.enabled || self.pending.is_empty() {
            return String::new();
        }
        let line = std::mem::take(&mut self.pending);
        self.render_line(&line)
    }

    /// Render one complete line (no trailing newline) to ANSI-styled text.
    fn render_line(&mut self, line: &str) -> String {
        let trimmed = line.trim_start();
        if let Some(ch) = fence_char(trimmed) {
            match self.fence {
                None => {
                    self.fence = Some(ch);
                    let info = trimmed.trim_start_matches(ch);
                    let lang = info.split_whitespace().next().unwrap_or("");
                    let syntax = SYNTAXES
                        .find_syntax_by_token(lang)
                        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());
                    self.hl = Some(HighlightLines::new(syntax, &THEME));
                    return String::new(); // fence lines are not printed
                }
                Some(open) if open == ch => {
                    self.fence = None;
                    self.hl = None;
                    return String::new();
                }
                Some(_) => {} // other fence char inside a block: literal content
            }
        }
        if let Some(hl) = &mut self.hl {
            let regions = hl.highlight_line(line, &SYNTAXES).unwrap_or_default();
            let mut s = as_24_bit_terminal_escaped(&regions[..], false);
            s.push_str(RESET);
            s
        } else {
            style_inline_code(line)
        }
    }
}

/// If `trimmed` opens with a code fence (3+ of the same '`' or '~'), the
/// fence character.
fn fence_char(trimmed: &str) -> Option<char> {
    let ch = trimmed.chars().next()?;
    if !matches!(ch, '`' | '~') {
        return None;
    }
    if trimmed.chars().take(3).eq(std::iter::repeat_n(ch, 3)) {
        Some(ch)
    } else {
        None
    }
}

/// Color inline `code` spans. An unbalanced backtick styles the rest of the
/// line — the span may continue on the next line in the raw markdown, but
/// per-line rendering can't know that, and coloring to end-of-line is the
/// less jarring failure mode.
fn style_inline_code(line: &str) -> String {
    if !line.contains('`') {
        return line.to_string();
    }
    let mut out = String::new();
    let mut in_code = false;
    for seg in line.split('`') {
        if in_code {
            out.push_str(INLINE_CODE);
            out.push_str(seg);
            out.push_str(RESET);
        } else {
            out.push_str(seg);
        }
        in_code = !in_code;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal_renderer() -> StreamRenderer {
        StreamRenderer {
            enabled: true,
            pending: String::new(),
            fence: None,
            hl: None,
        }
    }

    /// Remove ANSI SGR sequences; highlighting splits a line into separately
    /// styled regions, so raw source text is only contiguous after stripping.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(start) = rest.find("\x1b[") {
            out.push_str(&rest[..start]);
            match rest[start..].find('m') {
                Some(end) => rest = &rest[start + end + 1..],
                None => return out,
            }
        }
        out.push_str(rest);
        out
    }

    #[test]
    fn disabled_passes_through_unbuffered() {
        let mut r = StreamRenderer {
            enabled: false,
            pending: String::new(),
            fence: None,
            hl: None,
        };
        assert_eq!(r.push_str("partial"), "partial");
        assert_eq!(r.push_str("```rust\nlet x = 1;\n```\n"), "```rust\nlet x = 1;\n```\n");
        assert!(r.pending.is_empty());
        assert_eq!(r.finish_str(), "");
    }

    #[test]
    fn buffers_until_newline() {
        let mut r = terminal_renderer();
        assert_eq!(r.push_str("hel"), "");
        assert_eq!(r.push_str("lo"), "");
        assert_eq!(r.push_str(" world\nnext"), "hello world\n");
        assert_eq!(r.finish_str(), "next");
        assert_eq!(r.finish_str(), ""); // buffer drained
    }

    #[test]
    fn fence_lines_are_dropped_and_code_highlighted() {
        let mut r = terminal_renderer();
        let out = r.push_str("before\n```rust\nlet x = 1;\n```\nafter\n");
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines[0], "before");
        // Fence lines render as blank lines; the code line sits between them.
        assert_eq!(lines[1], "");
        assert_eq!(strip_ansi(lines[2]), "let x = 1;");
        assert!(lines[2].contains("\x1b["), "code line should be ANSI styled");
        assert!(lines[2].ends_with(RESET), "code line must end with a reset");
        assert_eq!(lines[3], "");
        assert_eq!(lines[4], "after");
        assert_eq!(r.fence, None);
    }

    #[test]
    fn unterminated_fence_still_highlights_on_flush() {
        let mut r = terminal_renderer();
        r.push_str("```python\nprint(1)\n");
        assert_eq!(r.push_str("print(2)"), ""); // partial line stays buffered
        let flushed = r.finish_str();
        assert_eq!(strip_ansi(&flushed), "print(2)");
        assert!(flushed.contains("\x1b["), "still inside the fence: styled");
    }

    #[test]
    fn inline_code_spans_are_colored() {
        let mut r = terminal_renderer();
        let out = r.push_str("use `foo()` here\n");
        assert!(out.contains(&format!("{INLINE_CODE}foo(){RESET}")));
        assert!(out.starts_with("use "));
        assert!(out.ends_with(" here\n"));
    }

    #[test]
    fn tilde_fences_and_info_attributes() {
        let mut r = terminal_renderer();
        let out = r.push_str("~~~rust ignore\nfn main() {}\n~~~\n");
        assert_eq!(strip_ansi(&out), "\nfn main() {}\n\n");
        assert!(!strip_ansi(&out).contains("~~~"), "fence lines dropped");
        assert_eq!(r.fence, None);
    }

    #[test]
    fn other_fence_char_inside_block_is_literal() {
        let mut r = terminal_renderer();
        let out = r.push_str("```\n~~~\n```\n");
        // The ~~~ line is code content (styled), not a closing fence.
        assert!(out.contains("~~~"));
        assert_eq!(r.fence, None);
    }

    #[test]
    fn detects_fence_chars() {
        assert_eq!(fence_char("```"), Some('`'));
        assert_eq!(fence_char("```rust"), Some('`'));
        assert_eq!(fence_char("~~~~"), Some('~'));
        assert_eq!(fence_char("``x"), None);
        assert_eq!(fence_char("not a fence"), None);
        assert_eq!(fence_char(""), None);
    }
}
