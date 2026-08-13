require "test_helper"

# The PWA surface: the manifest carries the app's identity (name, paper
# theme, installable icon set) and the service worker serves the asset-cache
# policy. Neither endpoint touches the bridge, so these tests need no stubs.
class PwaTest < ActionDispatch::IntegrationTest
  test "manifest serves the app identity and installable icons" do
    get pwa_manifest_path(format: :json)
    assert_response :success
    manifest = JSON.parse(response.body)

    assert_equal "Skiff", manifest["name"]
    assert_equal "standalone", manifest["display"]
    assert_equal "#f7f2e9", manifest["theme_color"]
    assert_equal "#f7f2e9", manifest["background_color"]

    sizes = manifest["icons"].map { |icon| icon["sizes"] }
    assert_includes sizes, "192x192"
    assert_includes sizes, "512x512"
    assert manifest["icons"].any? { |icon| icon["purpose"] == "maskable" }
  end

  test "service worker serves the asset-cache policy" do
    get pwa_service_worker_path(format: :js)
    assert_response :success
    assert_match "skiff-assets-v1", response.body
    assert_match "/offline.html", response.body
  end
end
