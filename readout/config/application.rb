require_relative "boot"

require "rails"
# Pick the frameworks you want:
require "active_model/railtie"
require "active_job/railtie"
require "active_record/railtie"
# require "active_storage/engine"
require "action_controller/railtie"
# require "action_mailer/railtie"
# require "action_mailbox/engine"
# require "action_text/engine"
require "action_view/railtie"
require "action_cable/engine"
require "rails/test_unit/railtie"

# Require the gems listed in Gemfile, including any gems
# you've limited to :test, :development, or :production.
Bundler.require(*Rails.groups)

module Readout
  class Application < Rails::Application
    # Initialize configuration defaults for originally generated Rails version.
    config.load_defaults 8.1

    # Please, add to the `ignore` list any other `lib` subdirectories that do
    # not contain `.rb` files, or that should not be reloaded or eager loaded.
    # Common ones are `templates`, `generators`, or `middleware`, for example.
    config.autoload_lib(ignore: %w[assets tasks])

    # Where the campfire-stress harness writes its results. Overridable so the
    # dashboard can read a copy of the results from somewhere else without
    # touching code.
    config.x.campfire_stress.results_root =
      ENV.fetch("CAMPFIRE_STRESS_RESULTS", File.expand_path("~/code/campfire-stress/results"))

    # The machines that can generate load, as JSON:
    #
    #   [{"key": "mac", "name": "MacBook", "url": "http://100.74.202.93:7881",
    #     "token_file": "/rails/config/runner-tokens/mac"}, ...]
    #
    # One value rather than a family of variables per machine, because the VPS
    # resolves these at service start (deploy/provision.sh) and writes the result
    # in one go — tailnet addresses are not constants, and a container there
    # cannot resolve MagicDNS to find them itself.
    #
    # Unset means the developer's case: the runner on this same box, over
    # loopback. See Harness::Fleet.default. A machine that is asleep simply does
    # not appear, and the dashboard offers no launch on it — the read-only half
    # keeps working regardless, which is the point.
    # Named literally rather than through Harness::Fleet::ENV_KEY: autoloading is
    # not available yet while this class body runs.
    config.x.runners = ENV.fetch("RUNNERS", nil)

    # The secret a generator machine presents when publishing a finished run.
    #
    # Results are parsed where the load was generated and pushed here, so this is
    # the one route into the app that writes without a person driving it. A file
    # rather than an env var, like the master key and the runner token: the
    # deploy ships an image tar over the network and systemd units are
    # world-readable, whereas this file is 0600 and mounted in.
    #
    # Missing is normal — a local instance nobody publishes to has no token, and
    # the endpoint then refuses everything. See IngestToken.
    config.x.ingest.token_file =
      ENV.fetch("INGEST_TOKEN_FILE", Rails.root.join("config/ingest-token").to_s)
  end
end
