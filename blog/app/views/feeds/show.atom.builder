# Every URL and every id here is built from the PUBLIC origin, never from the
# requesting host: this feed is fetched over the tailnet by the author as often
# as over the internet by a reader. URLs pointing at an internal hostname would
# be unreachable for everyone but him, and ids that varied by audience would
# make the whole feed look new on every switch. See ApplicationHelper#atom_tag.
atom_feed(language: "en-US",
          root_url: public_url(root_path),
          url: public_url(feed_path),
          id: atom_tag("/feed")) do |feed|
  feed.title(site_title)
  feed.updated(@updated_at)

  @posts.each do |post|
    feed.entry(post,
               url: public_url(post_path(post)),
               id: atom_tag("Post/#{post.id}"),
               published: post.published_at,
               updated: post.updated_at) do |entry|
      entry.title(post.title)
      entry.summary(post.excerpt, type: "text")

      # `to_s` renders the Action Text body to sanitized HTML. Attachments are
      # rendered with whatever partial they resolve to, so an image in a post
      # becomes an <img> pointing at an Active Storage URL — absolute and
      # publicly reachable, which is what a feed reader needs.
      entry.content(post.body.to_s, type: "html")
    end
  end
end
