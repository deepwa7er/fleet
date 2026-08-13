require "test_helper"

# Post bodies are Action Text (edited with Lexxy) rather than the markdown
# column they were until the Lexxy switch. These cover the properties the
# deleted MarkdownTest used to guarantee, now that a different layer guarantees
# them — most importantly that nothing executable survives to a public page.
class PostBodyTest < ActiveSupport::TestCase
  test "the body is Action Text, not a column" do
    post = Post.create!(title: "x", body: "<p>Hello.</p>")

    assert_kind_of ActionText::RichText, post.body
    assert_not Post.column_names.include?("body")
  end

  # ── sanitization: the property that matters on a public site ──────────────
  #
  # Action Text sanitizes on RENDER, not on write, so these assert against the
  # rendered output rather than the stored column.

  test "script tags do not survive rendering" do
    post = Post.create!(title: "x", body: "<p>Before</p><script>alert(1)</script>")

    assert_no_match(/<script/, post.body.to_s)
  end

  test "event handler attributes do not survive rendering" do
    post = Post.create!(title: "x", body: %(<p onclick="steal()">text</p>))

    assert_no_match(/onclick/, post.body.to_s)
  end

  test "javascript URLs do not survive rendering" do
    post = Post.create!(title: "x", body: %(<a href="javascript:alert(1)">click</a>))

    assert_no_match(/javascript:/, post.body.to_s)
  end

  test "iframes do not survive rendering" do
    post = Post.create!(title: "x", body: %(<iframe src="https://evil.test"></iframe>))

    assert_no_match(/<iframe/, post.body.to_s)
  end

  # Lexxy widens Action Text's allowlist so its own features round-trip. This
  # documents the widening rather than asserting it is harmless — a code block
  # that loses its language attribute stops being highlighted on the published
  # page, which is the failure this catches.
  test "code blocks keep their language attribute" do
    post = Post.create!(title: "x", body: %(<pre><code data-language="ruby">puts 1</code></pre>))

    assert_match(/data-language="ruby"/, post.body.to_s)
  end

  test "tables survive rendering" do
    post = Post.create!(title: "x", body: "<table><tbody><tr><td>a</td></tr></tbody></table>")

    assert_match(/<table/, post.body.to_s)
  end

  # ── plain-text derivations ────────────────────────────────────────────────

  test "summary_text falls back to the body's first paragraph" do
    post = Post.create!(title: "x", body: "<p>First para.</p><p>Second para.</p>")

    assert_equal "First para.", post.summary_text
  end

  test "summary_text still prefers an explicit summary" do
    post = Post.create!(title: "x", body: "<p>Body.</p>", summary: "Summary.")

    assert_equal "Summary.", post.summary_text
  end

  test "excerpt strips markup and collapses whitespace" do
    post = Post.create!(title: "x", body: "<h1>Title</h1>\n\n<p>Some <strong>bold</strong> prose.</p>")

    assert_equal "Title Some bold prose.", post.excerpt
  end

  test "excerpt truncates to the limit" do
    post = Post.create!(title: "x", body: "<p>#{'word ' * 500}</p>")

    assert_operator post.excerpt(limit: 100).length, :<=, 100
  end

  test "an empty body yields an empty excerpt and summary" do
    post = Post.create!(title: "x")

    assert_equal "", post.excerpt
    assert_equal "", post.summary_text
  end
end
