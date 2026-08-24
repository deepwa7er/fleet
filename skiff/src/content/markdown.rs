//! Markdown → typed blocks.
//!
//! A stack-based fold over pulldown-cmark's event stream: one stack of block
//! containers (the document, a quote, a list item) and one of inline
//! containers (a paragraph, an emphasis, a link). Every `Start` pushes and
//! every `End` pops into its parent, so an unbalanced stream degrades to
//! whatever parsed rather than panicking.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use super::{Block, Inline, highlight};

/// Parse `source` into blocks.
pub fn parse(source: &str) -> Vec<Block> {
    if source.trim().is_empty() {
        return Vec::new();
    }
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let mut builder = Builder::default();
    for event in Parser::new_ext(source, options) {
        builder.event(event);
    }
    builder.finish()
}

#[derive(Default)]
struct Builder {
    /// Stack of block containers; the bottom is the document itself.
    blocks: Vec<Vec<Block>>,
    /// Stack of inline containers. Non-empty exactly while inside a leaf block
    /// that holds inlines.
    inlines: Vec<Vec<Inline>>,
    lists: Vec<ListFrame>,
    tables: Vec<TableFrame>,
    heading: Vec<u8>,
    code: Vec<CodeFrame>,
    /// Link destinations, on their own stack so that nested links — which
    /// markdown forbids but a truncated stream can still produce — cannot
    /// cross wires.
    hrefs: Vec<String>,
}

struct ListFrame {
    ordered: bool,
    start: Option<u64>,
    items: Vec<Vec<Block>>,
}

#[derive(Default)]
struct TableFrame {
    head: Vec<Vec<Inline>>,
    rows: Vec<Vec<Vec<Inline>>>,
    row: Vec<Vec<Inline>>,
    in_head: bool,
}

struct CodeFrame {
    lang: Option<String>,
    source: String,
}

impl Builder {
    fn finish(mut self) -> Vec<Block> {
        // An unbalanced stream leaves frames open. A truncated stream is the
        // normal case here, not the exception — text arrives mid-structure on
        // every flush of a live reply — so closing outward is the main path,
        // and everything that parsed must survive it.

        // Unterminated inline containers flatten into their parent: an
        // unclosed `*` is emphasis that never happened, not lost words.
        while self.inlines.len() > 1 {
            let inner = self.inlines.pop().unwrap_or_default();
            if let Some(parent) = self.inlines.last_mut() {
                parent.extend(inner);
            }
        }
        // Inline content with no block around it — block-level raw HTML, or a
        // stream cut before its paragraph closed — becomes a paragraph rather
        // than being dropped.
        if let Some(inlines) = self.inlines.pop()
            && !inlines.is_empty()
        {
            self.push_block(Block::Paragraph { inlines });
        }

        while self.blocks.len() > 1 {
            let done = self.blocks.pop().unwrap_or_default();
            self.push_blocks(done);
        }
        self.blocks.pop().unwrap_or_default()
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(text) => self.push_inline(Inline::Code { text: text.into_string() }),
            // Raw HTML is literal text, never markup. See the module docs on
            // `content`: this is the property, not a downstream discipline.
            Event::Html(text) | Event::InlineHtml(text) => self.text(&text),
            // The renderer wraps, so a soft break is a space.
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.push_inline(Inline::Break),
            Event::Rule => self.push_block(Block::Rule),
            // Footnotes, task markers, and math have no rendering yet; they
            // are dropped rather than rendered as their source.
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.inlines.push(Vec::new()),
            Tag::Heading { level, .. } => {
                self.heading.push(level as u8);
                self.inlines.push(Vec::new());
            }
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    // "```rust,ignore" — the language is the first word.
                    CodeBlockKind::Fenced(info) => info
                        .split(&[',', ' '][..])
                        .next()
                        .filter(|word| !word.is_empty())
                        .map(str::to_owned),
                    CodeBlockKind::Indented => None,
                };
                self.code.push(CodeFrame { lang, source: String::new() });
            }
            Tag::List(start) => self.lists.push(ListFrame {
                ordered: start.is_some(),
                start,
                items: Vec::new(),
            }),
            Tag::Item => self.blocks.push(Vec::new()),
            Tag::BlockQuote(_) => self.blocks.push(Vec::new()),
            Tag::Table(_) => self.tables.push(TableFrame::default()),
            Tag::TableHead => {
                if let Some(table) = self.tables.last_mut() {
                    table.in_head = true;
                }
            }
            Tag::TableRow => {}
            Tag::TableCell => self.inlines.push(Vec::new()),
            Tag::Emphasis | Tag::Strong | Tag::Strikethrough => self.inlines.push(Vec::new()),
            Tag::Link { dest_url, .. } => {
                self.inlines.push(Vec::new());
                self.hrefs.push(dest_url.into_string());
            }
            // An image has no transport in a transcript; its alt text is kept
            // as prose, which is what a reader can actually use.
            Tag::Image { .. } => self.inlines.push(Vec::new()),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                let inlines = self.inlines.pop().unwrap_or_default();
                if !inlines.is_empty() {
                    self.push_block(Block::Paragraph { inlines });
                }
            }
            TagEnd::Heading(_) => {
                let inlines = self.inlines.pop().unwrap_or_default();
                let level = self.heading.pop().unwrap_or(1);
                self.push_block(Block::Heading { level, inlines });
            }
            TagEnd::CodeBlock => {
                if let Some(frame) = self.code.pop() {
                    let tokens = highlight::tokenize(frame.lang.as_deref(), &frame.source);
                    self.push_block(Block::Code { lang: frame.lang, tokens });
                }
            }
            TagEnd::List(_) => {
                if let Some(frame) = self.lists.pop() {
                    self.push_block(Block::List {
                        ordered: frame.ordered,
                        // A list starting at 1 is the default; carrying it
                        // would make every ordered list look customised.
                        start: frame.start.filter(|n| *n != 1),
                        items: frame.items,
                    });
                }
            }
            TagEnd::Item => {
                let mut blocks = self.blocks.pop().unwrap_or_default();
                // A tight list's item holds bare inlines rather than a
                // paragraph; normalise so the renderer sees one shape.
                if blocks.is_empty()
                    && let Some(inlines) = self.inlines.pop()
                    && !inlines.is_empty()
                {
                    blocks.push(Block::Paragraph { inlines });
                }
                if let Some(list) = self.lists.last_mut() {
                    list.items.push(blocks);
                }
            }
            TagEnd::BlockQuote(_) => {
                let blocks = self.blocks.pop().unwrap_or_default();
                self.push_block(Block::Quote { blocks });
            }
            TagEnd::Table => {
                if let Some(frame) = self.tables.pop() {
                    self.push_block(Block::Table { head: frame.head, rows: frame.rows });
                }
            }
            TagEnd::TableHead => {
                if let Some(table) = self.tables.last_mut() {
                    table.in_head = false;
                    table.head = std::mem::take(&mut table.row);
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = self.tables.last_mut() {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            TagEnd::TableCell => {
                let cell = self.inlines.pop().unwrap_or_default();
                if let Some(table) = self.tables.last_mut() {
                    table.row.push(cell);
                }
            }
            TagEnd::Emphasis => {
                let inlines = self.inlines.pop().unwrap_or_default();
                self.push_inline(Inline::Emph { inlines });
            }
            TagEnd::Strong => {
                let inlines = self.inlines.pop().unwrap_or_default();
                self.push_inline(Inline::Strong { inlines });
            }
            TagEnd::Strikethrough => {
                let inlines = self.inlines.pop().unwrap_or_default();
                self.push_inline(Inline::Strike { inlines });
            }
            TagEnd::Link => {
                let inlines = self.inlines.pop().unwrap_or_default();
                let href = self.hrefs.pop().unwrap_or_default();
                self.push_inline(Inline::Link { href, inlines });
            }
            TagEnd::Image => {
                let inlines = self.inlines.pop().unwrap_or_default();
                for inline in inlines {
                    self.push_inline(inline);
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if let Some(frame) = self.code.last_mut() {
            frame.source.push_str(text);
            return;
        }
        self.push_inline(Inline::Text { text: text.to_owned() });
    }

    /// Append an inline, merging adjacent plain text so a paragraph is not a
    /// list of one-word fragments.
    fn push_inline(&mut self, inline: Inline) {
        let Some(container) = self.inlines.last_mut() else {
            // Inline content outside any block — pulldown-cmark does not
            // normally emit this, but a truncated stream can. Open a paragraph
            // rather than dropping the text.
            self.inlines.push(vec![inline]);
            return;
        };
        if let (Inline::Text { text }, Some(Inline::Text { text: last })) =
            (&inline, container.last_mut())
        {
            last.push_str(text);
            return;
        }
        container.push(inline);
    }

    fn push_block(&mut self, block: Block) {
        if self.blocks.is_empty() {
            self.blocks.push(Vec::new());
        }
        if let Some(container) = self.blocks.last_mut() {
            container.push(block);
        }
    }

    fn push_blocks(&mut self, blocks: Vec<Block>) {
        for block in blocks {
            self.push_block(block);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::TokenClass;

    fn text(s: &str) -> Inline {
        Inline::Text { text: s.to_owned() }
    }

    #[test]
    fn a_paragraph_is_one_block_of_inlines() {
        assert_eq!(parse("hello world"), [Block::Paragraph { inlines: vec![text("hello world")] }]);
    }

    #[test]
    fn blank_input_is_no_blocks_rather_than_an_empty_paragraph() {
        assert!(parse("").is_empty());
        assert!(parse("   \n\n  ").is_empty());
    }

    #[test]
    fn emphasis_and_links_nest_as_inlines() {
        let blocks = parse("a *b* and [c](http://x)");
        let Block::Paragraph { inlines } = &blocks[0] else { panic!("{blocks:?}") };
        assert_eq!(inlines[0], text("a "));
        assert_eq!(inlines[1], Inline::Emph { inlines: vec![text("b")] });
        assert_eq!(inlines[2], text(" and "));
        assert_eq!(
            inlines[3],
            Inline::Link { href: "http://x".into(), inlines: vec![text("c")] }
        );
    }

    #[test]
    fn adjacent_text_is_merged_into_one_inline() {
        // pulldown-cmark splits text at entity and break boundaries; a
        // paragraph should not arrive as a list of fragments.
        let blocks = parse("one two three");
        let Block::Paragraph { inlines } = &blocks[0] else { panic!() };
        assert_eq!(inlines.len(), 1);
    }

    #[test]
    fn a_soft_break_becomes_a_space_and_a_hard_break_is_kept() {
        let Block::Paragraph { inlines } = &parse("a\nb")[0] else { panic!() };
        assert_eq!(inlines, &[text("a b")]);

        let Block::Paragraph { inlines } = &parse("a  \nb")[0] else { panic!() };
        assert_eq!(inlines, &[text("a"), Inline::Break, text("b")]);
    }

    #[test]
    fn a_fenced_block_carries_its_language_and_is_highlighted() {
        let blocks = parse("```rust\nfn f() {}\n```");
        let Block::Code { lang, tokens } = &blocks[0] else { panic!("{blocks:?}") };
        assert_eq!(lang.as_deref(), Some("rust"));
        assert!(tokens.iter().any(|t| t.class == TokenClass::Keyword));
        let joined: String = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, "fn f() {}\n");
    }

    #[test]
    fn a_fence_info_string_yields_only_its_first_word_as_the_language() {
        let Block::Code { lang, .. } = &parse("```rust,ignore\nx\n```")[0] else { panic!() };
        assert_eq!(lang.as_deref(), Some("rust"));
    }

    #[test]
    fn an_unfenced_block_has_no_language_and_is_not_highlighted() {
        let Block::Code { lang, tokens } = &parse("    indented code\n")[0] else { panic!() };
        assert_eq!(*lang, None);
        assert_eq!(tokens.iter().map(|t| t.class).collect::<Vec<_>>(), [TokenClass::Plain]);
    }

    #[test]
    fn inline_code_is_never_highlighted() {
        let Block::Paragraph { inlines } = &parse("call `fn main()` now")[0] else { panic!() };
        assert_eq!(inlines[1], Inline::Code { text: "fn main()".into() });
    }

    #[test]
    fn a_tight_list_item_is_normalised_to_hold_a_paragraph() {
        let blocks = parse("- one\n- two\n");
        let Block::List { ordered, start, items } = &blocks[0] else { panic!("{blocks:?}") };
        assert!(!ordered);
        assert_eq!(*start, None);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], vec![Block::Paragraph { inlines: vec![text("one")] }]);
    }

    #[test]
    fn a_loose_list_item_keeps_its_blocks() {
        let blocks = parse("- one\n\n- two\n");
        let Block::List { items, .. } = &blocks[0] else { panic!() };
        assert_eq!(items[0], vec![Block::Paragraph { inlines: vec![text("one")] }]);
    }

    #[test]
    fn an_ordered_list_carries_a_start_only_when_it_is_not_one() {
        let Block::List { ordered, start, .. } = &parse("1. a\n2. b\n")[0] else { panic!() };
        assert!(ordered);
        assert_eq!(*start, None, "starting at 1 is the default, not a customisation");

        let Block::List { start, .. } = &parse("7. a\n8. b\n")[0] else { panic!() };
        assert_eq!(*start, Some(7));
    }

    #[test]
    fn a_nested_list_lives_inside_its_parent_item() {
        let blocks = parse("- outer\n  - inner\n");
        let Block::List { items, .. } = &blocks[0] else { panic!("{blocks:?}") };
        assert!(matches!(items[0].last(), Some(Block::List { .. })), "got {:?}", items[0]);
    }

    #[test]
    fn a_quote_holds_blocks() {
        let blocks = parse("> quoted\n");
        let Block::Quote { blocks: inner } = &blocks[0] else { panic!("{blocks:?}") };
        assert_eq!(inner, &[Block::Paragraph { inlines: vec![text("quoted")] }]);
    }

    #[test]
    fn a_table_separates_its_head_from_its_rows() {
        let blocks = parse("| a | b |\n|---|---|\n| 1 | 2 |\n");
        let Block::Table { head, rows } = &blocks[0] else { panic!("{blocks:?}") };
        assert_eq!(head, &[vec![text("a")], vec![text("b")]]);
        assert_eq!(rows, &[vec![vec![text("1")], vec![text("2")]]]);
    }

    #[test]
    fn headings_carry_their_level() {
        let blocks = parse("# one\n\n### three\n");
        assert_eq!(blocks[0], Block::Heading { level: 1, inlines: vec![text("one")] });
        assert_eq!(blocks[2 - 1], Block::Heading { level: 3, inlines: vec![text("three")] });
    }

    #[test]
    fn raw_html_is_literal_text_and_never_markup() {
        // The property, not a downstream discipline: model output that
        // contains a tag must reach the client as characters.
        let blocks = parse("<script>alert(1)</script>");
        let rendered = format!("{blocks:?}");
        assert!(rendered.contains("<script>"), "the tag survives as text: {rendered}");
        assert!(!rendered.contains("Html"), "and never as a markup node");
    }

    #[test]
    fn an_image_degrades_to_its_alt_text() {
        let Block::Paragraph { inlines } = &parse("![a diagram](x.png)")[0] else { panic!() };
        assert_eq!(inlines, &[text("a diagram")]);
    }

    #[test]
    fn a_truncated_stream_yields_what_did_parse() {
        // The normal case while a reply streams: the source ends mid-structure.
        let blocks = parse("- one\n- two");
        assert!(matches!(blocks.first(), Some(Block::List { .. })), "{blocks:?}");

        let blocks = parse("```rust\nfn f() {");
        let Block::Code { tokens, .. } = &blocks[0] else { panic!("{blocks:?}") };
        let joined: String = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, "fn f() {");
    }

    #[test]
    fn a_stream_cut_mid_emphasis_keeps_the_words_and_the_marker() {
        // CommonMark resolves an unmatched `*` to a literal asterisk, so the
        // partial reply reads as it was written rather than losing a character
        // that is about to become emphasis on the next flush.
        let Block::Paragraph { inlines } = &parse("a *bold")[0] else { panic!() };
        assert_eq!(inlines, &[text("a *bold")]);
    }

    #[test]
    fn a_thematic_break_is_its_own_block() {
        assert_eq!(parse("a\n\n---\n\nb")[1], Block::Rule);
    }
}
