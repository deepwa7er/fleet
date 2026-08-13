require "cgi"
require "rouge"

# Highlights the code blocks in a rendered post body.
#
# WHY THIS IS SERVER-SIDE:
#
# Lexxy highlights in the browser, with Prism, out of the 933KB bundle that also
# carries the editor — and that bundle is imported by admin.js alone, because a
# reader needs nothing but Turbo. So the published page has been serving code as
# plain text on the fill surface while the token palette in application.css
# styled only the admin. Doing it in Ruby fixes that without asking a reader to
# download an editor, and it happens inside the fragment cache the body already
# sits in: once per edit, not once per reader.
#
# WHAT IT DOES NOT DO:
#
# It does not touch a block Lexxy did not label, and it does not guess a
# language. An unlabelled or unknown block stays exactly as written — plain text
# is a correct rendering of code, and a wrong guess is not.
class SyntaxHighlighter
  # Rouge emits the token spans alone, with no wrapper of its own, which is what
  # lets the highlighted content drop straight into the <code> element Lexxy
  # already wrote.
  FORMATTER = Rouge::Formatters::HTML.new

  # Nothing to gain from marking up text that has no tokens, and the spans would
  # only be noise in the cached HTML.
  UNHIGHLIGHTED = %w[ plain plaintext text txt ].freeze

  def self.highlight(html) = new(html).to_html

  def initialize(html)
    @html = html.to_s
  end

  def to_html
    # Returned untouched when there is nothing to do, rather than round-tripped
    # through a parser for no reason: reserialising well-formed HTML is safe but
    # not free, and most posts have no code in them at all.
    return @html unless @html.include?("<pre")

    fragment = Nokogiri::HTML5.fragment(@html)
    blocks = fragment.css("pre[data-language]")
    return @html if blocks.empty?

    blocks.each { |block| highlight_block(block) }
    fragment.to_html
  end

  private

  # The code inside a block, as text, with its lines intact.
  #
  # Lexxy separates the lines of a code block with <br> elements rather than
  # newlines, and Nokogiri's #text drops elements — so reading it directly hands
  # the lexer one enormous line. Every language suffers from that; a diff is
  # simply where it shows, because its lexer reads the first character of each
  # line and there is only one line to read. That is why a whole diff came out
  # uncoloured.
  #
  # Done on a copy: the block still has to carry its original markup into the
  # replacement below.
  def source_text(block)
    copy = block.dup
    copy.css("br").each { |line_break| line_break.replace(Nokogiri::XML::Text.new("\n", copy.document)) }
    copy.text
  end

  def highlight_block(block)
    language = block["data-language"].to_s.downcase.strip
    return if UNHIGHLIGHTED.include?(language)

    lexer = Rouge::Lexer.find(language)
    return if lexer.nil?

    # Read before writing: the source is the block's text, and replacing the
    # markup first would leave nothing to read.
    source = source_text(block)
    target = block.at_css("code") || block

    target.inner_html =
      if language == "diff"
        diff_html(source)
      else
        FORMATTER.format(lexer.new.lex(source))
      end

    # The same marker Lexxy's own highlighter sets, so if Prism is ever loaded
    # on a page carrying this HTML it leaves the block alone instead of
    # highlighting it a second time.
    block["data-highlighted"] = "true"
  end

  # A diff line's role, by the prefix that gives it away. Order matters twice
  # over: `--- ` and `+++ ` are file headers and must be recognised before the
  # plain `-` and `+` that would otherwise claim them, and the trailing space is
  # what separates a header from content — a removed line that reads `---`
  # arrives as `----` and stays a removal.
  DIFF_ROLES = [
    [ /\A(?:diff --git |index |--- |\+\+\+ )/, "gh" ],
    [ /\A@@/,                                  "gu" ],
    [ /\A\+/,                                  "gi" ],
    [ /\A-/,                                   "gd" ]
  ].freeze

  # A diff, rendered one line at a time.
  #
  # WHY NOT ROUGE, WHEN ROUGE HAS A DIFF LEXER:
  #
  # Two reasons, and the second is the one that matters. It reads `--- a/file`
  # as a removed line and `+++ b/file` as an added one, so every diff opened
  # with a red line and a green line describing changes that never happened.
  # And it groups each RUN of same-role lines into one inline span, which
  # cannot carry a line's worth of anything: a background stops at the text
  # rather than the width of the block, and nothing lines up. A diff is read by
  # scanning a column of markers, so the line has to be the unit.
  #
  # The lexer was doing prefix matching either way. Doing it here is less code
  # than correcting it afterwards, and every line becomes its own block.
  def diff_html(source)
    source.lines.map { |line| diff_line(line) }.join
  end

  def diff_line(line)
    text = line.chomp
    _, role = DIFF_ROLES.find { |pattern, _| text.match?(pattern) }

    # A blank line still occupies one, or the column of markers develops gaps
    # that are not in the diff.
    content = text.empty? ? "&nbsp;" : CGI.escapeHTML(text)

    %(<span class="#{[ "dl", role ].compact.join(" ")}">#{content}</span>)
  end
end
