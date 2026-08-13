module ApplicationHelper
  def site_title = Rails.application.config.x.site_title

  # True when this request came in on the admin hostname. Used only to decide
  # what the masthead offers — it is NOT a security check. Authorization is the
  # router's host constraint plus Admin::BaseController; a helper that a view
  # can forget to call would be a poor place to put a boundary.
  def admin_context? = request.host == Rails.application.config.x.admin_host

  # Absolute URL on the public origin. See config/application.rb for why the
  # feed and the canonical tag must not use the requesting host.
  def public_url(path)
    base = Rails.application.config.x.public_base_url.presence || request.base_url
    "#{base}#{path}"
  end

  # A permanent Atom identifier.
  #
  # Rails derives both the feed's <id> and each entry's <id> from `request.host`
  # by default, which is wrong for this app: the same feed is fetched over the
  # tailnet by the author and over the internet by everyone else, so the ids
  # would differ by audience. An Atom id is the permanent key a reader uses to
  # decide whether it has seen an entry before — if it changes, every reader is
  # told the entire feed is new. Deriving it from the public host, which never
  # varies with the request, is what makes it permanent.
  #
  # 2005 is the Atom spec's copyright date and Rails' own default schema date;
  # it is part of the tag URI format, not a timestamp, so it must never change.
  def atom_tag(suffix)
    "tag:#{Rails.application.config.x.public_host},2005:#{suffix}"
  end

  # Metadata reads as instrumentation (DW-001 rule 5): uppercase, letterspaced,
  # tabular. Dates are rendered unambiguously — 05 AUG 2026, never 08/05 —
  # because a readout should not need to know which side of the Atlantic wrote it.
  def readout_date(time)
    return nil if time.blank?

    tag.time(time.strftime("%d %b %Y").upcase, datetime: time.iso8601, class: "meta")
  end

  # The state of a post, said in words. Colour is a second signal here, never
  # the only one — the label carries the meaning on its own.
  def post_state(post)
    if post.scheduled?
      tag.span("Scheduled #{post.published_at.strftime('%d %b %Y').upcase}", class: "state state--scheduled")
    elsif post.published?
      tag.span("Published", class: "state state--published")
    else
      tag.span("Draft", class: "state state--draft")
    end
  end
end
