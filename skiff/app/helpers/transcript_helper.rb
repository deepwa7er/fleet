module TranscriptHelper
  # DW-001 §8 discipline: assistant prose is the one place LLM output becomes
  # page markup, so the renderer is pinned to unsafe: false (raw HTML escaped —
  # verified against commonmarker 2.9.0's default; explicit here so an upstream
  # default flip cannot silently start injecting raw HTML) with the syntax
  # highlighter plugin off and github_pre_lang: false. That keeps the app's own
  # pre/code surface (§7 log/program-output) in charge: github_pre_lang: true
  # would render lang= on the pre element, and the default highlighter injects
  # inline styles that fight the --fill surface. Never cached: a transcript
  # must always reflect the latest streamed text.
  #
  # Markdown output gets one more pass on top of commonmarker's: decorate_markdown
  # wraps each pre in the code-block surface (header with the language label and
  # a Copy key, then the block), highlights known languages with the token
  # palette — mapped to the app's own status colors, never a second palette (the
  # diff precedent, DW-001 §8) — and wraps wide tables in a scroll box so the
  # page itself never scrolls sideways (§4).
  def render_markdown(text)
    text = text.to_s
    return "" if text.blank?

    html = Commonmarker.to_html(
      text,
      options: { extension: { table: true }, render: { github_pre_lang: false, unsafe: false } },
      plugins: { syntax_highlighter: nil }
    )
    decorate_markdown(html)
  end

  # DW-001 §2: --danger marks a failed state. A tool line turns bad only for
  # statuses that mean the call did not complete; running/completed stay muted.
  def tool_line_bad?(part)
    %w[error failed].include?(part.dig(:state, :status).to_s)
  end

  private

  # One pass over commonmarker's HTML: every pre becomes the code-block
  # surface — a header row (language label in instrumentation voice, Copy key)
  # above the block itself — and every table gets a scroll wrapper, so the
  # page itself never scrolls sideways (anything wide scrolls inside its own
  # overflow-x box, §4). A fenced block's language class on the code element
  # is the label and the lexer hint; indented blocks have neither and get the
  # surface and the Copy key alone. An unknown language is labeled but not
  # highlighted — honest metadata, no guessing. The pass is pure markup: it
  # never touches inline code, prose, or headings.
  def decorate_markdown(html)
    doc = Nokogiri::HTML5.fragment(html)
    doc.css("pre").each do |pre|
      code = pre.at_css("> code")
      next unless code

      language = code["class"].to_s[/language-(\S+)/, 1]
      if language && (lexer = Rouge::Lexer.find_fancy(language))
        code.inner_html = SyntaxTokenFormatter.new.format(lexer.lex(code.text))
      end

      # Replace the pre with the surface, then move the pre into it — the
      # other order would make the surface an ancestor of the node being
      # replaced, which Nokogiri rejects as a cycle.
      block = code_block_node(pre, language)
      pre.replace(block)
      block.add_child(pre)
    end
    doc.css("table").each do |table|
      wrapper = Nokogiri::HTML5.fragment('<div class="table-scroll"></div>').at_css("div")
      table.replace(wrapper)
      wrapper.add_child(table)
    end
    doc.to_html.html_safe
  end

  def code_block_node(pre, language)
    pre["data-code-block-target"] = "source"
    label = language ? %(<span class="instrumentation">#{CGI.escapeHTML(language)}</span>) : ""
    Nokogiri::HTML5.fragment(<<~HTML).at_css(".code-block")
      <div class="code-block" data-controller="code-block">
        <div class="code-block-header">
          #{label}
          <button type="button" class="button button--small button--secondary" data-action="code-block#copy">Copy</button>
        </div>
      </div>
    HTML
  end

  # Maps Rouge's token taxonomy onto the app's existing status palette — the
  # diff resolution's precedent (DW-001 §8: three roles, no second palette):
  # keywords take the one accent, strings and inserted lines take --good,
  # deleted lines take --danger, comments take muted ink. Everything else
  # stays the block's ink, unspanned.
  class SyntaxTokenFormatter < Rouge::Formatter
    # Rouge 5 yields token class objects; matches? asks whether the token
    # class descends from the mapped family (e.g. Keyword::Declaration).
    TOKEN_CLASSES = {
      Rouge::Token::Tokens::Keyword => "tok-keyword",
      Rouge::Token::Tokens::Literal::String => "tok-string",
      Rouge::Token::Tokens::Comment => "tok-comment",
      Rouge::Token::Tokens::Generic::Deleted => "tok-deleted",
      Rouge::Token::Tokens::Generic::Inserted => "tok-inserted"
    }.freeze

    def stream(tokens)
      tokens.each do |token, value|
        escaped = CGI.escapeHTML(value)
        klass = TOKEN_CLASSES.find { |type, _| type.matches?(token) }&.last
        yield klass ? %(<span class="#{klass}">#{escaped}</span>) : escaped
      end
    end
  end
end
