require "test_helper"

# rouge.css is generated, and the generating command is a comment at the top of
# it. Regenerating without the scope would put GitHub's green and red bands
# back behind every diff line — silently, because nothing else would fail.
class RougeThemeTest < ActiveSupport::TestCase
  THEME = Rails.root.join("app/assets/stylesheets/rouge.css")

  test "the generated theme cannot reach a diff" do
    css = THEME.read

    assert_no_match(
      /^\.prose pre \./, css,
      "every rule must be scoped :not([data-language=\"diff\"]) — a diff is coloured by " \
      "line role in application.css, per §8, and a second palette reaching those lines " \
      "is what put background bands on them"
    )
  end

  # The reason the scope matters: the theme does carry diff colours, and they
  # are backgrounds.
  test "the theme still carries the diff colours it must be kept away from" do
    css = THEME.read

    assert_match(/:not\(\[data-language="diff"\]\) \.gi \{/, css)
    assert_match(/background-color/, css)
  end

  test "both modes are present, because the site has both" do
    css = THEME.read

    assert_match(/@media \(prefers-color-scheme: dark\)/, css)
  end
end
