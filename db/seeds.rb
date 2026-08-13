# Seeds a single post exercising the constructs Lexxy produces and the
# stylesheet claims to handle — headings, prose, links, lists, a quote, a table,
# inline code, and a fenced code block with a language. It exists so that a
# fresh checkout can be looked at, not merely booted.
#
# The body is HTML rather than markdown: bodies are Action Text now, and this is
# roughly what Lexxy writes. `data-language` on the <code> is what its
# highlighter keys off, and what the published page needs in order to colour the
# block the same way the editor did.
#
# Idempotent: re-running updates the same slug rather than colliding with the
# unique index.
#
# NOTE: db:prepare runs this automatically when it CREATES a database, so a
# fresh production deploy publishes this post. That is how it ended up on the
# live site the first time. Delete it once there is something real to read.

post = Post.find_or_initialize_by(slug: "hello")

post.assign_attributes(
  title: "Hello",
  summary: "A first post, and a test of everything this blog claims to render.",
  published_at: post.published_at || Time.current
)

post.body = <<~HTML
  <p>This blog runs on the deepwater style: cream paper, warm ink, one Bavarian
  blue, and no dividing lines anywhere. Hierarchy comes from whitespace and
  typography — the gap between two things <em>is</em> the separator.</p>

  <h2>What it renders</h2>

  <p>Ordinary prose, with <strong>strong</strong> and <em>emphasised</em> text,
  <code>inline code</code>, and <a href="https://example.com">links</a> in the one
  accent colour the palette allows.</p>

  <p>Lists work the way you would expect:</p>

  <ul>
    <li>Whitespace separates content.</li>
    <li>Depth marks interactivity.</li>
    <li>Metadata is instrumentation.</li>
  </ul>

  <blockquote>
    <p>A quotation is set off by space and a change of voice, not by the
    conventional left border. Rule 1 does not make exceptions.</p>
  </blockquote>

  <h3>Code</h3>

  <p>Code blocks sit on the fill surface and scroll themselves, so the page never
  scrolls sideways:</p>

  <pre><code data-language="ruby">class Post &lt; ApplicationRecord
    has_rich_text :body

    scope :published, -&gt; { where(published_at: ..Time.current) }
  end</code></pre>

  <h3>Tables</h3>

  <p>Tables get no rules and no zebra striping — the gaps carry the grouping, and
  every column header is instrumentation:</p>

  <table>
    <thead>
      <tr><th>Token</th><th>Light</th><th>Dark</th></tr>
    </thead>
    <tbody>
      <tr><td>bg</td><td><code>#f7f2e9</code></td><td><code>#121316</code></td></tr>
      <tr><td>text</td><td><code>#1f1a12</code></td><td><code>#eceef1</code></td></tr>
      <tr><td>accent</td><td><code>#0066b1</code></td><td><code>#4d9de0</code></td></tr>
    </tbody>
  </table>

  <p>That is the whole system.</p>
HTML

post.save!

puts "Seeded #{Post.count} post(s)."
