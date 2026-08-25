require "test_helper"

# The home page's masthead title and its heading are the same string, so it is
# said once — as the incipit, in the masthead's place (DW-001 G5). A page never
# says its title twice.
class PagesControllerTest < ActionDispatch::IntegrationTest
  test "the home page says the site title once, as the incipit" do
    get "/"

    assert_response :success
    assert_match %r{<h1 class="incipit">deepwa7er</h1>}, response.body
    assert_no_match(/class="wordmark"/, response.body)
  end

  # Away from home the wordmark is navigation back to it, so it stays.
  test "the masthead keeps the wordmark link on non-home pages" do
    get "/forum"

    assert_response :success
    assert_match(/class="wordmark"/, response.body)
  end
end
