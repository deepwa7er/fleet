require "test_helper"

# The admin's writing surface. These are thin, but they are the regression net
# for the Lexxy swap: `rich_text_area` renders a <trix-editor> unless Lexxy has
# successfully prepended itself to Action Text's tag helper, and that failure is
# silent — the form still works, it is just the wrong editor.
class AdminEditorTest < ActionDispatch::IntegrationTest
  setup do
    host! Rails.application.config.x.admin_host
    @post = Post.create!(title: "A post", body: "<p>Body.</p>", published_at: 1.day.ago)
  end

  test "the new-post form renders a Lexxy editor, not Trix" do
    get "/admin/posts/new"

    assert_response :success
    assert_match(/<lexxy-editor/, response.body)
    assert_no_match(/<trix-editor/, response.body)
  end

  test "the edit form renders a Lexxy editor carrying the current body" do
    get "/admin/posts/#{@post.slug}/edit"

    assert_response :success
    assert_match(/<lexxy-editor/, response.body)
    assert_match "Body.", response.body
  end

  test "creating a post through the form stores a rich text body" do
    assert_difference -> { Post.count } do
      post "/admin/posts", params: {
        post: { title: "Written in Lexxy", body: "<p>Some <strong>prose</strong>.</p>" }
      }
    end

    created = Post.find_by!(slug: "written-in-lexxy")
    assert_kind_of ActionText::RichText, created.body
    assert_match(/<strong>prose<\/strong>/, created.body.to_s)
  end

  test "editing the body updates it" do
    patch "/admin/posts/#{@post.slug}", params: { post: { body: "<p>Rewritten.</p>" } }

    assert_redirected_to "/admin/posts/#{@post.slug}"
    assert_match "Rewritten.", @post.reload.body.to_s
  end

  # ── the asset split ───────────────────────────────────────────────────────
  #
  # lexxy.js is 933 KB and powers the editor only. These assert it is loaded for
  # the author and not for the public — a split that CANNOT be checked in
  # development, where both hostnames are localhost and every page looks like
  # the admin.

  test "the admin loads the editor entry point and the full Lexxy stylesheet" do
    get "/admin/posts/new"

    assert_match(/import "admin"/, response.body)
    assert_match(/lexxy-[0-9a-f]+\.css/, response.body)
  end

  test "the public site loads neither the editor entry point nor the editor CSS" do
    host! Rails.application.config.x.public_host

    get "/posts/#{@post.slug}"

    assert_response :success
    assert_match(/import "application"/, response.body)
    assert_no_match(/import "admin"/, response.body)

    # lexxy-content.css yes (it styles a rendered post), bare lexxy.css no.
    assert_match(/lexxy-content-[0-9a-f]+\.css/, response.body)
    assert_no_match(/lexxy-[0-9a-f]+\.css/, response.body)
  end

  # Preloading is on by default for every pin, which would defeat the split
  # entirely: the browser would fetch lexxy.js on the public site even though
  # nothing there imports it.
  test "lexxy is never modulepreloaded on the public site" do
    host! Rails.application.config.x.public_host

    get "/"

    assert_no_match(/modulepreload[^>]*lexxy/, response.body)
  end

  # The published page caches on [post, post.body]. Editing only the body does
  # not touch the posts row, so a cache key built from the post alone would keep
  # serving the previous text — this asserts the fact that makes the key correct.
  test "editing only the body changes the body's cache key" do
    before = @post.body.cache_key_with_version

    @post.update!(body: "<p>Changed.</p>")

    assert_not_equal before, @post.reload.body.cache_key_with_version
  end
end
