module PostsHelper
  # The body as a reader sees it: Action Text's own rendering — attachments
  # resolved, HTML sanitized — with its code blocks highlighted afterwards.
  #
  # Highlighting runs AFTER sanitizing, deliberately. The spans it adds are
  # generated from an escaped token stream rather than from anything a writer
  # typed, so they widen nothing; running it first would only hand the sanitizer
  # more markup to strip, and it strips class attributes it does not know.
  # `to_s` on the rich text is what `<%= post.body %>` was already emitting:
  # Action Text's content layout, its attachment partials, and its sanitizer.
  # This adds one step to the end of that, and changes nothing about it.
  def rendered_post_body(post)
    raw SyntaxHighlighter.highlight(post.body.to_s)
  end
end
