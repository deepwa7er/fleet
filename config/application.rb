require_relative "boot"

require "rails"
# Pick the frameworks you want:
require "active_model/railtie"
require "active_job/railtie"
require "active_record/railtie"
require "active_storage/engine"
require "action_controller/railtie"
require "action_mailer/railtie"
# require "action_mailbox/engine"
require "action_text/engine"
require "action_view/railtie"
# require "action_cable/engine"
require "rails/test_unit/railtie"

# Require the gems listed in Gemfile, including any gems
# you've limited to :test, :development, or :production.
Bundler.require(*Rails.groups)

module Blog
  class Application < Rails::Application
    # Initialize configuration defaults for originally generated Rails version.
    config.load_defaults 8.1

    # Please, add to the `ignore` list any other `lib` subdirectories that do
    # not contain `.rb` files, or that should not be reloaded or eager loaded.
    # Common ones are `templates`, `generators`, or `middleware`, for example.
    config.autoload_lib(ignore: %w[assets tasks])

    # Configuration for the application, engines, and railties goes here.
    #
    # These settings can be overridden in specific environments using the files
    # in config/environments, which are processed later.
    #
    # config.time_zone = "Central Time (US & Canada)"
    # config.eager_load_paths << Rails.root.join("extras")

    # ── The two hostnames this app answers to ────────────────────────────────
    #
    # One process serves two audiences, and the hostname is what separates them:
    #
    #   public_host  the internet, read-only. Fronted by nginx on the VPS.
    #   admin_host   the tailnet, read/write. Fronted by breakwater.
    #
    # This is the app's security boundary, so it is worth being precise about
    # what makes it hold. The routes for /admin are drawn inside a host
    # constraint (see config/routes.rb), which means they do not exist at all
    # for a request arriving on the public host — the router 404s before any
    # controller runs. That constraint reads `request.host`, which comes from
    # the Host header, which a client controls. It is therefore only as good as
    # the proxy in front: the public nginx vhost pins `proxy_set_header Host`
    # to a literal (NOT `$host`), so a request to blog.deepwa7er.com carrying a
    # forged `Host: blog.intern.deepwa7er.net` reaches this app with the
    # hostname rewritten to the public one and still finds no admin routes.
    #
    # Admin::BaseController re-checks the same fact independently, so the
    # boundary survives a routing refactor that loses the constraint.
    #
    # Both default to localhost so development is a single host with the admin
    # reachable — there is nothing to separate on a laptop.
    config.x.public_host = ENV.fetch("BLOG_PUBLIC_HOST", "localhost")
    config.x.admin_host  = ENV.fetch("BLOG_ADMIN_HOST", "localhost")

    # Absolute base for links that must point at the public site regardless of
    # which host served the page: canonical tags, and every URL in the feed. A
    # feed fetched through the tailnet must still advertise public URLs, or the
    # entries it hands a reader are unreachable from the internet.
    #
    # Left nil in development, where the helper falls back to the request's own
    # base URL — there is no separate public origin on a laptop.
    config.x.public_base_url = ENV.fetch("BLOG_PUBLIC_BASE_URL", nil)

    # Shown in the masthead and the feed. Set per-environment rather than
    # hardcoded in a view so the feed and the page cannot disagree.
    config.x.site_title  = ENV.fetch("BLOG_SITE_TITLE", "deepwater")
  end
end
