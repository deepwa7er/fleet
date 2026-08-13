require "test_helper"

# The skiff_env initializer loads /Users/deepwater/.config/skiff/secrets into
# ENV at boot. Hermetic: skip on machines where the real file is absent (CI),
# and never write to it.
class SkiffEnvTest < ActionDispatch::IntegrationTest
  SECRETS_FILE = "/Users/deepwater/.config/skiff/secrets"

  test "loads the real secrets file into ENV" do
    skip "skiff secrets file missing" unless File.exist?(SECRETS_FILE)

    loaded = File.readlines(SECRETS_FILE).filter_map do |line|
      line = line.strip
      next if line.empty? || line.start_with?("#")

      key, value = line.split("=", 2)
      [ key, value ] if key && value
    end.to_h

    assert loaded.key?("OPENCODE_SERVER_PASSWORD"), "secrets file lacks OPENCODE_SERVER_PASSWORD"
    loaded.each do |key, value|
      assert_equal value, ENV[key], "ENV[#{key}] should be loaded from secrets"
    end
  end
end
