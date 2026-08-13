require "test_helper"

# DW-001 §8: the markdown helper is the one place LLM output becomes page
# markup, so its tests pin the escaping behavior (unsafe: false — the literal
# tag must never survive) and the code-block shape: every pre becomes the
# code-block surface with a header (language label + Copy key), and known
# languages are highlighted with the palette token classes — never inline
# styles, so the --fill surface and dark mode stay the app's.
class TranscriptHelperTest < ActionView::TestCase
  test "renders fenced code blocks as a code-block surface with header and copy key" do
    html = render_markdown("```ruby\nputs 1\n```")
    assert_includes html, "<div class=\"code-block\""
    assert_includes html, "data-controller=\"code-block\""
    assert_includes html, "data-code-block-target=\"source\""
    assert_includes html, ">ruby</span>"
    assert_includes html, ">Copy</button>"
  end

  test "highlights known languages with palette token classes, never inline styles" do
    html = render_markdown("```ruby\ndef hi\n  \"s\"\n  # note\nend\n```")
    assert_includes html, "tok-keyword" # def / end
    assert_includes html, "tok-string"
    assert_includes html, "tok-comment"
    refute_includes html, "style="
  end

  test "labels an unknown language without highlighting it" do
    html = render_markdown("```plain\nraw text\n```")
    assert_includes html, ">plain</span>"
    refute_includes html, "tok-"
  end

  test "indented code blocks get the surface and copy key without a label" do
    html = render_markdown("    indented line")
    assert_includes html, "<div class=\"code-block\""
    assert_includes html, ">Copy</button>"
    refute_includes html, "class=\"instrumentation\""
    refute_includes html, "tok-"
  end

  test "inline code is untouched" do
    html = render_markdown("inline `code` here")
    assert_includes html, "<code>code</code>"
    refute_includes html, "code-block"
  end

  test "code text stays escaped inside fenced blocks" do
    html = render_markdown("```html\n<script>alert(1)</script>\n```")
    refute_includes html, "<script>"
    assert_includes html, "&lt;script&gt;"
  end

  test "renders tables when the table extension is on, wrapped so the page never scrolls sideways" do
    html = render_markdown("| a | b |\n|---|--:|\n| 1 | 2 |")
    assert_includes html, "<div class=\"table-scroll\">"
    assert_includes html, "<table>"
    assert_includes html, "<th align=\"right\">b</th>"
  end

  test "escapes raw HTML so a script never reaches the page" do
    html = render_markdown("<script>alert('x')</script>")
    refute_includes html, "<script>"
  end

  test "blank text renders nothing" do
    assert_equal "", render_markdown("")
    assert_equal "", render_markdown(nil)
    assert_equal "", render_markdown("   ")
  end
end
