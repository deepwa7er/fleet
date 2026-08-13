require "test_helper"

class PostTest < ActiveSupport::TestCase
  test "generates a slug from the title on create" do
    post = Post.create!(title: "A Post About Things", body: "x")

    assert_equal "a-post-about-things", post.slug
  end

  test "appends a counter when the slug is taken" do
    Post.create!(title: "Year in review", body: "x")
    second = Post.create!(title: "Year in review", body: "x")

    assert_equal "year-in-review-2", second.slug
  end

  test "does not change the slug when the title changes" do
    post = Post.create!(title: "Original", body: "x")
    post.update!(title: "Renamed")

    assert_equal "original", post.reload.slug
  end

  test "rejects a slug that is not lowercase hyphenated" do
    post = Post.new(title: "x", body: "x", slug: "Not A Slug")

    assert_not post.valid?
    assert_includes post.errors[:slug].first, "lowercase"
  end

  test "requires a title" do
    assert_not Post.new(body: "x").valid?
  end

  # ── published / draft / scheduled ─────────────────────────────────────────

  test "a post with no published_at is a draft" do
    post = Post.create!(title: "Draft", body: "x")

    assert_predicate post, :draft?
    assert_not post.published?
    assert_not post.scheduled?
  end

  test "a post dated in the past is published" do
    post = Post.create!(title: "Live", body: "x", published_at: 1.hour.ago)

    assert_predicate post, :published?
    assert_not post.draft?
  end

  # The scope compares against the clock rather than merely checking the column
  # for NULL, so a future date is not yet public. This is what keeps a date set
  # from the console from publishing something early.
  test "a post dated in the future is scheduled, not published" do
    post = Post.create!(title: "Later", body: "x", published_at: 1.day.from_now)

    assert_predicate post, :scheduled?
    assert_not post.published?
    assert_not_includes Post.published, post
  end

  test "the published scope excludes drafts and future posts" do
    live = Post.create!(title: "Live", body: "x", published_at: 1.hour.ago)
    Post.create!(title: "Draft", body: "x")
    Post.create!(title: "Later", body: "x", published_at: 1.day.from_now)

    assert_equal [ live ], Post.published.to_a
  end

  # ── summary fallback ──────────────────────────────────────────────────────

  test "summary_text prefers the explicit summary" do
    post = Post.create!(title: "x", body: "The body.", summary: "The summary.")

    assert_equal "The summary.", post.summary_text
  end

  test "summary_text falls back to the first paragraph" do
    post = Post.create!(title: "x", body: "First para.\n\nSecond para.")

    assert_equal "First para.", post.summary_text
  end

  test "to_param is the slug" do
    post = Post.create!(title: "Some Title", body: "x")

    assert_equal "some-title", post.to_param
  end
end
