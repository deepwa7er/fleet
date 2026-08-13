require "test_helper"

# This app serves two audiences from one process, and the hostname is the only
# thing separating them: the internet gets read-only routes, the tailnet gets
# the admin. These tests are the regression net for that boundary — if one of
# them fails, the admin is on the public internet.
#
# See config/application.rb for why the Host header is trustworthy here (the
# public nginx vhost pins it to a literal), and Admin::BaseController for the
# second, independent check that backs up the router's host constraint.
class HostBoundaryTest < ActionDispatch::IntegrationTest
  setup do
    @post = Post.create!(title: "A post", body: "Body.", published_at: 1.day.ago)
  end

  # ── the admin is unreachable from the public hostname ─────────────────────

  test "admin index is not found on the public host" do
    host! Rails.application.config.x.public_host

    get "/admin"
    assert_response :not_found
  end

  test "every admin route is not found on the public host" do
    host! Rails.application.config.x.public_host

    [
      [ :get,  "/admin/posts" ],
      [ :get,  "/admin/posts/new" ],
      [ :get,  "/admin/posts/#{@post.slug}" ],
      [ :get,  "/admin/posts/#{@post.slug}/edit" ],
      [ :post, "/admin/posts" ],
      [ :patch, "/admin/posts/#{@post.slug}" ],
      [ :delete, "/admin/posts/#{@post.slug}" ],
      [ :post, "/admin/posts/#{@post.slug}/publish" ],
      [ :post, "/admin/posts/#{@post.slug}/unpublish" ]
    ].each do |verb, path|
      send(verb, path)
      assert_response :not_found, "#{verb.to_s.upcase} #{path} was reachable on the public host"
    end
  end

  # A write attempted from the public host must not merely be refused — it must
  # not happen. This is the test that would catch a boundary that 404s the read
  # but lets the side effect through.
  test "a destroy from the public host does not delete the post" do
    host! Rails.application.config.x.public_host

    assert_no_difference -> { Post.count } do
      delete "/admin/posts/#{@post.slug}"
    end
    assert_response :not_found
  end

  # ── the admin IS reachable from the admin hostname ────────────────────────

  test "admin index is served on the admin host" do
    host! Rails.application.config.x.admin_host

    get "/admin"
    assert_response :success
  end

  test "admin can reach a post and edit it" do
    host! Rails.application.config.x.admin_host

    get "/admin/posts/#{@post.slug}"
    assert_response :success

    patch "/admin/posts/#{@post.slug}", params: { post: { title: "Renamed" } }
    assert_redirected_to "/admin/posts/#{@post.slug}"
    assert_equal "Renamed", @post.reload.title
  end

  # ── the second line of defence, checked on its own ────────────────────────
  #
  # Admin::BaseController re-verifies the host independently of the router, so
  # that a routing refactor which drops the `constraints` block does not
  # silently publish the admin. Exercised directly, because in normal operation
  # the router gets there first and this code never runs.

  test "the controller check rejects a public host even when the router does not" do
    controller = Admin::PostsController.new
    controller.set_request!(ActionDispatch::TestRequest.create(
      "HTTP_HOST" => Rails.application.config.x.public_host
    ))

    assert_raises(ActionController::RoutingError) do
      controller.send(:require_admin_host)
    end
  end

  test "the controller check accepts the admin host" do
    controller = Admin::PostsController.new
    controller.set_request!(ActionDispatch::TestRequest.create(
      "HTTP_HOST" => Rails.application.config.x.admin_host
    ))

    assert_nil controller.send(:require_admin_host)
  end

  # ── the public site works on the public host ──────────────────────────────

  test "public routes are served on the public host" do
    host! Rails.application.config.x.public_host

    get "/"
    assert_response :success

    get "/posts/#{@post.slug}"
    assert_response :success

    get "/feed"
    assert_response :success
  end
end
