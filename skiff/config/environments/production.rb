require "active_support/core_ext/integer/time"

Rails.application.configure do
  # Settings specified here will take precedence over those in config/application.rb.

  # Code is not reloaded between requests.
  config.enable_reloading = false

  # Eager load code on boot for better performance and memory savings (ignored by Rake tasks).
  config.eager_load = true

  # Full error reports are disabled.
  config.consider_all_requests_local = false

  # Turn on fragment caching in view templates.
  config.action_controller.perform_caching = true

  # Cache assets for far-future expiry since they are all digest stamped.
  config.public_file_server.headers = { "cache-control" => "public, max-age=#{1.year.to_i}" }

  # Enable serving of images, stylesheets, and JavaScripts from an asset server.
  # config.asset_host = "http://assets.example.com"

  # TLS is terminated by breakwater on the tailnet, which forwards plain HTTP
  # to Rails — so the app must treat every production request as "already
  # HTTPS" and generate https URLs behind the proxy (assume_ssl). force_ssl
  # stays off because breakwater itself 308-redirects HTTP, and direct tailnet
  # http access (http://fedora.tailcfab97.ts.net:8120) still works without an
  # upgrade.
  config.assume_ssl = true
  config.force_ssl = false

  # Log to STDOUT with the current request id as a default log tag.
  config.log_tags = [ :request_id ]
  config.logger   = ActiveSupport::TaggedLogging.logger(STDOUT)

  # Change to "debug" to log everything (including potentially personally-identifiable information!).
  config.log_level = ENV.fetch("RAILS_LOG_LEVEL", "info")

  # Prevent health checks from clogging up the logs.
  config.silence_healthcheck_path = "/up"

  # Don't log any deprecations.
  config.active_support.report_deprecations = false

  # Replace the default in-process memory cache store with a durable alternative.
  # config.cache_store = :mem_cache_store

  # Replace the default in-process and non-durable queuing backend for Active Job.
  # config.active_job.queue_adapter = :resque

  # Enable locale fallbacks for I18n (makes lookups for any locale fall back to
  # the I18n.default_locale when a translation cannot be found).
  config.i18n.fallbacks = true

  # Enable DNS rebinding protection and other `Host` header attacks.
  #
  # Tailnet-only posture: skiff is served to the human over the tailnet
  # (the phone), and must keep working when poked at locally on
  # localhost/127.0.0.1 (health checks, dev smoke tests). The app deploys to
  # either of two tailnet nodes — the Mac (deepwater-1.tailcfab97.ts.net) and
  # the Fedora desktop (fedora.tailcfab97.ts.net) — so both MagicDNS names are
  # allowed, and Host Authorization still blocks every other host. Setting
  # config.hosts explicitly in production activates Host Authorization — the
  # default empty array would skip the middleware entirely and accept any Host
  # header, which is exactly what we do not want for a headless server.
  config.hosts = [
    "agent.intern.deepwa7er.net",  # legacy hostname — hand-written tailnet route (fedora), kept until VPS cutover
    "skiff.intern.deepwa7er.net",  # breakwater GENERATED route for the fleet monorepo deploy (127.0.0.1:8120)
    "localhost",
    "127.0.0.1",
    "deepwater-1.tailcfab97.ts.net",
    "fedora.tailcfab97.ts.net"
  ]
  #
  # Skip DNS rebinding protection for the default health check endpoint.
  # config.host_authorization = { exclude: ->(request) { request.path == "/up" } }
end
