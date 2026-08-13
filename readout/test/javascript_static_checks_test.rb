require "test_helper"

# Static checks on the JavaScript, because its failures are invisible from here.
#
# There is no browser in this suite, so nothing below actually runs the code. Each
# check reads it instead, looking for the specific mistakes that leave every
# server-side signal healthy — the page renders, the asset is served, the JSON is
# correct — while the feature does nothing in the browser.
#
# The original of that kind follows.
#
# `bin/importmap pin <package>` downloads whatever the CDN serves, and popular
# packages ship code-split ESM builds: the vendored file imports
# "./chunks/something.js" or "../_/<hash>.js", and importmap does not fetch those
# chunks. Every check that can be made server-side still passes — the pin exists,
# the asset is served with a 200, the page references it — and the module then
# fails to resolve in the browser, so the feature silently never works.
#
# Chart.js was vendored here for exactly one release and hit exactly this. It has
# since been dropped (the live chart draws itself on a canvas; the static charts
# are server-rendered SVG), so there may be nothing vendored at all — which is
# fine. This exists so the next dependency is checked automatically.
class JavascriptStaticChecksTest < ActiveSupport::TestCase
  IMPORT_PATTERN = /(?:^|\s)(?:import|export)\s*(?:\{[^}]*\}|\*|\w+)?\s*from\s*['"]([^'"]+)['"]/

  test "vendored javascript, if any, has no unresolved module specifiers" do
    files = Dir[Rails.root.join("vendor/javascript/**/*.js")]

    files.each do |path|
      specifiers = File.read(path).scan(IMPORT_PATTERN).flatten.uniq

      assert_empty specifiers,
        "#{File.basename(path)} imports #{specifiers.inspect}, which importmap will not " \
        "resolve in the browser. Vendor a self-contained bundle instead."
    end
  end

  # Anything a controller imports must actually be pinned, or the module fails to
  # load and the controller never runs.
  test "controller imports resolve to something the importmap knows about" do
    pinned = File.read(Rails.root.join("config/importmap.rb"))
      .scan(/^\s*pin\s+["']([^"']+)["']/).flatten
    pinned << "controllers" # pin_all_from covers the controllers directory

    Dir[Rails.root.join("app/javascript/controllers/*.js")].each do |path|
      File.read(path).scan(/from\s+["']([^"'.][^"']*)["']/).flatten.each do |specifier|
        next if specifier.start_with?("controllers/")

        assert pinned.any? { |p| specifier == p || specifier.start_with?("#{p}/") },
          "#{File.basename(path)} imports #{specifier.inspect}, which is not pinned in " \
          "config/importmap.rb — it will fail to load in the browser."
      end
    end
  end

  # Methods Stimulus itself provides, which a controller may call without
  # defining. Kept short deliberately: anything else named here would weaken the
  # check into a formality.
  STIMULUS_METHODS = %w[dispatch].freeze

  # A method a controller calls on itself but never defines.
  #
  # The live chart called this.formatDelay for days without it existing. The
  # throw happened inside the animation frame, so nothing reached the console a
  # person would look at, and the canvas simply stopped updating — which looks
  # exactly like a chart with no data. Every server-side check passed: the run
  # was healthy, the payload had 592 points, the markup and the asset were
  # correct.
  #
  # This cannot see dynamic dispatch (this[name]()), and it does not need to:
  # nothing here uses it, and the mistake it exists to catch is the plain call to
  # a name that was never written.
  test "controllers define every method they call on themselves" do
    Dir[Rails.root.join("app/javascript/controllers/*.js")].each do |path|
      source = File.read(path)

      # Method definitions sit at one level of indentation inside the class, and
      # may carry any of the usual modifiers.
      defined = source.scan(/^\s{2}(?:(?:static|async|get|set)\s+)*(\w+)\s*\(/).flatten
      called = source.scan(/\bthis\.(\w+)\(/).flatten.uniq

      missing = called - defined - STIMULUS_METHODS

      assert_empty missing,
        "#{File.basename(path)} calls #{missing.inspect} on itself but never defines " \
        "it. In the browser that throws mid-frame, which reads as a feature that " \
        "quietly does nothing."
    end
  end
end
