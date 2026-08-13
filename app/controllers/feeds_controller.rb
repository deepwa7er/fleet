# Atom feed. Atom rather than RSS 2.0 because it requires an unambiguous,
# stable id per entry and a real timestamp format, both of which this app
# already has (the slug and published_at).
class FeedsController < ApplicationController
  def show
    @posts = Post.published.recent.limit(50)

    # `updated` on the feed itself must be the newest entry's timestamp, not
    # "now" — a feed whose updated time changes on every fetch tells every
    # reader it has new content on every poll.
    @updated_at = @posts.first&.published_at || Time.at(0).utc

    respond_to do |format|
      format.atom
    end
  end
end
