class Post < ApplicationRecord
  # The body is Action Text, edited with Lexxy. It was a markdown column until
  # db/migrate/*_convert_post_bodies_to_rich_text.rb; nothing in the app parses
  # markdown at runtime any more.
  #
  # Action Text sanitizes on render, so there is no Markdown module doing it by
  # hand now. Lexxy widens the allowlist (video/audio/table tags, plus a `style`
  # attribute) — see Lexxy::Engine — which is a broader surface than the old
  # markdown pipeline permitted. Acceptable because the only writer reaches the
  # admin over the tailnet, but it is a real widening and worth knowing about
  # for a page served to the public.
  has_rich_text :body

  # The slug is the public URL. It is generated from the title on create and
  # then left alone: retitling a post must not break links that already exist
  # in the wild, so changing a slug is an explicit act in the admin form rather
  # than a side effect of editing the title.
  SLUG_FORMAT = /\A[a-z0-9]+(?:-[a-z0-9]+)*\z/

  validates :title, presence: true
  validates :slug, presence: true, uniqueness: true, format: {
    with: SLUG_FORMAT,
    message: "must be lowercase words separated by single hyphens"
  }

  before_validation :assign_slug, on: :create

  # `published_at` in the future means scheduled, not live — so "published" is a
  # question about the clock, never just about the column being set.
  scope :published, -> { where(published_at: ..Time.current) }
  scope :drafts,    -> { where(published_at: nil) }
  scope :recent,    -> { order(published_at: :desc, created_at: :desc) }

  def to_param = slug

  def published? = published_at.present? && published_at <= Time.current

  def scheduled? = published_at.present? && published_at > Time.current

  def draft? = published_at.nil?

  # Falls back to the body's opening paragraph so the index never shows a blank
  # entry for a post whose author did not write a summary.
  #
  # `to_plain_text` rather than parsing the stored HTML: Action Text already
  # knows how to flatten its own markup, including attachments, which a regex
  # over the HTML would either mangle or leak.
  def summary_text
    return summary if summary.present?

    body.to_plain_text.to_s.split(/\n\s*\n/).first.to_s.strip
  end

  # Plain text for the feed's <summary>. An excerpt in a feed should not carry
  # half-open markup, and truncating rendered HTML would do exactly that.
  def excerpt(limit: 400)
    body.to_plain_text.to_s.gsub(/\s+/, " ").strip.truncate(limit, separator: " ")
  end

  private

  # Appends -2, -3, … on collision. Two posts may legitimately share a title
  # (an annual "year in review", say), and the database's unique index would
  # otherwise turn that into a save failure the author cannot fix from the form.
  def assign_slug
    return if slug.present?

    base = title.to_s.parameterize
    return if base.blank?

    candidate = base
    suffix = 1
    candidate = "#{base}-#{suffix += 1}" while Post.exists?(slug: candidate)
    self.slug = candidate
  end
end
