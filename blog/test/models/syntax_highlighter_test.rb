require "test_helper"

# Highlighting happens here rather than in the browser, because Lexxy's
# highlighter ships with the editor and a reader never downloads it.
#
# The properties worth holding are: it colours what Lexxy labelled, it refuses
# to guess at what Lexxy did not, and nothing a writer types can escape the
# code block as markup.
class SyntaxHighlighterTest < ActiveSupport::TestCase
  def highlight(html) = SyntaxHighlighter.highlight(html)

  test "a labelled block is tokenised" do
    html = highlight(%(<pre data-language="ruby"><code>def hello\n  "world"\nend</code></pre>))

    assert_match(/<span class="k">def<\/span>/, html)
    assert_match(/class="s2"/, html, "a string literal is its own token")
    assert_match(/data-highlighted="true"/, html)
  end

  # Three roles and nothing else, per §8 of the style guide: Rouge's diff lexer
  # tags whole lines and does not tokenise inside them, which is exactly what
  # that ruling asks for.
  test "a diff is coloured by line role" do
    diff = <<~DIFF
      diff --git a/app/models/room.rb b/app/models/room.rb
      @@ -77,7 +77,7 @@
       def unread_memberships(message)
      -  memberships.visible.update_all(unread_at: message.created_at)
      +  memberships.visible.where("unread_at < ?", 5.seconds.ago).update_all(unread_at: message.created_at)
       end
    DIFF

    html = highlight(%(<pre data-language="diff"><code>#{diff}</code></pre>))

    assert_match(/<span class="dl gd">-  memberships/, html, "a removed line")
    assert_match(/<span class="dl gi">\+  memberships/, html, "an added line")
    assert_match(/<span class="dl gh">diff --git/, html, "the file header")
  end

  # A wrong guess is worse than no guess: plain text is a correct rendering of
  # code, and a language the author did not choose is not.
  test "a block Lexxy did not label is left alone" do
    html = highlight("<pre><code>def hello; end</code></pre>")

    assert_equal "<pre><code>def hello; end</code></pre>", html
  end

  test "a language Rouge does not know is left alone" do
    html = highlight(%(<pre data-language="klingon"><code>def hello; end</code></pre>))

    assert_no_match(/<span/, html)
    assert_no_match(/data-highlighted/, html)
  end

  test "plain text is not marked up for the sake of it" do
    %w[ plain plaintext text txt ].each do |language|
      html = highlight(%(<pre data-language="#{language}"><code>just words</code></pre>))

      assert_no_match(/<span/, html, "#{language} needs no tokens")
    end
  end

  # The reason highlighting runs after Action Text has sanitized: whatever it
  # emits has to be safe to mark html_safe. Rouge escapes as it formats, so code
  # that looks like markup stays code.
  test "code that looks like markup is escaped, not rendered" do
    html = highlight(%(<pre data-language="ruby"><code>puts "&lt;script&gt;alert(1)&lt;/script&gt;"</code></pre>))

    assert_no_match(/<script/, html)
    assert_match(/&lt;script&gt;/, html)
  end

  test "a body with no code at all comes back untouched" do
    body = "<p>Just prose, and an <em>emphasis</em>.</p>"

    assert_equal body, highlight(body)
  end

  test "prose around a code block survives" do
    html = highlight(%(<p>Before</p><pre data-language="ruby"><code>1 + 1</code></pre><p>After</p>))

    assert_match(/<p>Before<\/p>/, html)
    assert_match(/<p>After<\/p>/, html)
  end

  test "several blocks in one post are each highlighted in their own language" do
    html = highlight(
      %(<pre data-language="ruby"><code>def a; end</code></pre>) +
      %(<pre data-language="diff"><code>+added\n-gone\n</code></pre>)
    )

    assert_match(/<span class="k">def<\/span>/, html)
    assert_match(/<span class="dl gi">\+added/, html)
  end

  # Rouge reads `--- a/file` as a removed line and `+++ b/file` as an added one,
  # because to a line-prefix lexer that is what they look like. Left alone, a
  # diff opens with a red line and a green line describing changes that did not
  # happen — and Rouge groups each header with the real run that follows it, so
  # this cannot be fixed in CSS.
  test "a diff's file headers are headers, not changes" do
    diff = <<~DIFF
      diff --git a/config/database.yml b/config/database.yml
      --- a/config/database.yml
      +++ b/config/database.yml
      @@ -27,6 +27,10 @@
       performance:
      +      cache_size: -64000
      +      busy_timeout: 5000
      DIFF

    html = highlight(%(<pre data-language="diff"><code>#{diff}</code></pre>))

    assert_match(%r{<span class="dl gh">--- a/config/database\.yml}, html)
    assert_match(%r{<span class="dl gh">\+\+\+ b/config/database\.yml}, html)

    # And the real additions that followed the header keep their own role.
    assert_match(/<span class="dl gi">\+      cache_size: -64000/, html)
    assert_no_match(%r{<span class="dl gi">\+\+\+ b/}, html)
    assert_no_match(%r{<span class="dl gd">--- a/}, html)
  end

  # The distinguishing character is the space. A removed line that reads `---`
  # arrives as `----`, and is a change like any other.
  test "a removed line that looks like a file header is still a removal" do
    html = highlight(%(<pre data-language="diff"><code>@@ -1 +1 @@\n----\n+++++\n</code></pre>))

    assert_match(/<span class="dl gd">----/, html)
    assert_no_match(/<span class="dl gh">/, html)
  end

  # ── the markup Lexxy actually writes ──────────────────────────────────────
  #
  # These are the tests that were missing. Every case above uses "\n" between
  # lines, which is what a fixture written by hand looks like — but Lexxy
  # separates the lines of a code block with <br>, and Nokogiri's #text drops
  # elements. So the lexer was handed one enormous line, and a real diff pasted
  # into a real post came out with no colour at all while the suite stayed green.

  test "lines separated by br are lines" do
    body = %(<pre data-language="diff"><code>@@ -1,2 +1,2 @@<br>-gone<br>+added<br></code></pre>)

    html = highlight(body)

    assert_match(/<span class="dl gd">-gone/, html)
    assert_match(/<span class="dl gi">\+added/, html)
  end

  test "a language block written with br highlights every line, not the first" do
    body = %(<pre data-language="ruby"><code>def hello<br>  "world"<br>end</code></pre>)

    html = highlight(body)

    assert_match(/<span class="k">def<\/span>/, html)
    assert_match(/<span class="k">end<\/span>/, html, "the last line is reached too")
  end

  # Lexxy writes a tab inside its own element; the text has to survive that.
  test "tabs written as elements survive into the source" do
    body = %(<pre data-language="diff"><code>@@ -1 +1 @@<br>+a<span>\t</span>b<br></code></pre>)

    html = highlight(body)

    assert_match(/<span class="dl gi">\+a\tb/, html)
  end

  # A diff is read by scanning the column of markers down its left edge, so the
  # LINE is the unit: each is its own block, which is what lets a role's colour
  # cover the line rather than stopping where the text does. Rouge groups whole
  # runs of same-role lines into one inline span and can do neither.
  test "every line of a diff is its own block, including context and blanks" do
    diff = "@@ -1,4 +1,4 @@<br> context<br>-gone<br><br>+added<br>"

    html = highlight(%(<pre data-language="diff"><code>#{diff}</code></pre>))

    assert_equal 5, html.scan(/class="dl/).length, "one block per line"
    assert_match(/<span class="dl"> context<\/span>/, html, "a context line is a line too")
    assert_match(/<span class="dl">&nbsp;<\/span>/, html, "a blank line still occupies one")
  end

  test "the hunk header is its own role, not a heading and not a change" do
    html = highlight(%(<pre data-language="diff"><code>@@ -1 +1 @@<br>+x<br></code></pre>))

    assert_match(/<span class="dl gu">@@ -1 \+1 @@<\/span>/, html)
  end

  # Only diffs are rendered line by line; a language block stays Rouge's.
  test "a language block is not broken into diff lines" do
    html = highlight(%(<pre data-language="ruby"><code>def a<br>end</code></pre>))

    assert_no_match(/class="dl/, html)
  end
end
