require "test_helper"

# What the internet can see. The theme running through these is that an
# unpublished post must be indistinguishable from one that never existed.
class PublicSiteTest < ActionDispatch::IntegrationTest
  setup do
    host! Rails.application.config.x.public_host

    @live  = Post.create!(title: "Live post", body: "Live body.", published_at: 2.days.ago)
    @draft = Post.create!(title: "Draft post", body: "Draft body.")
    @later = Post.create!(title: "Later post", body: "Later body.", published_at: 1.day.from_now)
  end

  test "the index lists published posts only" do
    get "/"

    assert_response :success
    assert_match "Live post", response.body
    assert_no_match(/Draft post/, response.body)
    assert_no_match(/Later post/, response.body)
  end

  # The home page's masthead title and its heading are the same string, so it is
  # said once — as the incipit, in the masthead's place. A page never says its
  # title twice.
  test "the home page says the site title once, as the incipit" do
    get "/"

    assert_response :success
    assert_match %r{<h1 class="incipit">deepwater</h1>}, response.body
    assert_no_match(/class="wordmark"/, response.body)
  end

  # Away from home the wordmark is navigation back to it, so it stays.
  test "the masthead keeps the wordmark link on non-home pages" do
    get "/posts/#{@live.slug}"

    assert_response :success
    assert_match(/class="wordmark"/, response.body)
  end

  test "a published post renders its body" do
    get "/posts/#{@live.slug}"

    assert_response :success
    assert_match "Live body.", response.body
  end

  # G5 + G11: the post's title is the incipit of its page — the same blackletter
  # display voice and illuminated first letter the home page gives the site title.
  test "a post's title is rendered as the page's incipit" do
    get "/posts/#{@live.slug}"

    assert_response :success
    assert_match %r{<h1 class="incipit">Live post</h1>}, response.body
  end

  test "a draft is a 404, not a 403" do
    get "/posts/#{@draft.slug}"

    assert_response :not_found
  end

  test "a scheduled post is a 404 until its date passes" do
    get "/posts/#{@later.slug}"
    assert_response :not_found

    travel_to 2.days.from_now do
      get "/posts/#{@later.slug}"
      assert_response :success
    end
  end

  test "an unknown slug is a 404" do
    get "/posts/no-such-post"

    assert_response :not_found
  end

  # ── the feed ──────────────────────────────────────────────────────────────

  test "the feed contains published posts only" do
    get "/feed"

    assert_response :success
    assert_equal "application/atom+xml", response.media_type
    assert_match "Live post", response.body
    assert_no_match(/Draft post/, response.body)
  end

  # A feed fetched over the tailnet must still advertise public URLs, or every
  # link it hands a reader is unreachable from the internet.
  test "feed entry links use the public base URL even when served to another host" do
    host! Rails.application.config.x.admin_host

    get "/feed"

    assert_response :success
    assert_match "https://public.test/posts/#{@live.slug}", response.body
    assert_no_match(/admin\.test/, response.body)
  end

  # An Atom id is the permanent key a reader uses to recognise an entry it has
  # already shown. Rails derives it from request.host by default, which would
  # give the same post two different identities depending on which hostname
  # served the feed — and make every entry look brand new whenever the author
  # fetched it over the tailnet.
  test "feed and entry ids do not vary with the requesting host" do
    get "/feed"
    public_body = response.body

    host! Rails.application.config.x.admin_host
    get "/feed"

    assert_equal public_body, response.body
    assert_match "tag:public.test,2005:/feed", public_body
    assert_match "tag:public.test,2005:Post/#{@live.id}", public_body
  end

  test "the feed's updated time is the newest entry, not now" do
    get "/feed"

    assert_match @live.published_at.utc.iso8601, response.body
  end

  test "the feed is empty but valid with no published posts" do
    Post.delete_all

    get "/feed"
    assert_response :success
  end

  # ── health ────────────────────────────────────────────────────────────────

  # tugboat's deploy health check curls this; a 200 here is what stops a bad
  # build from being rolled back.
  test "the health endpoint is public" do
    get "/up"

    assert_response :success
  end

  # Code reaches a reader highlighted, from the server. Lexxy's highlighter is
  # in the editor bundle, which the public page deliberately never loads — so
  # without this, published code is plain text on the fill surface while the
  # token palette styles only the admin.
  test "a published post's code block arrives highlighted" do
    post = Post.create!(
      title: "With code",
      body: %(<p>Look:</p><pre data-language="ruby"><code>def hello\n  "world"\nend</code></pre>),
      published_at: 1.day.ago
    )

    get "/posts/#{post.slug}"

    assert_response :success
    assert_match(/<span class="k">def<\/span>/, response.body)
    assert_no_match(/lexxy\.js/, response.body, "a reader must not be sent the editor to read code")
  end

  test "a published diff arrives coloured by line role" do
    post = Post.create!(
      title: "With a diff",
      body: %(<pre data-language="diff"><code>@@ -1 +1 @@\n-gone\n+added\n</code></pre>),
      published_at: 1.day.ago
    )

    get "/posts/#{post.slug}"

    assert_match(/<span class="dl gd">-gone/, response.body)
    assert_match(/<span class="dl gi">\+added/, response.body)
  end
end
