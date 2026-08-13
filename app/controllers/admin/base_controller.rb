module Admin
  # Everything under /admin inherits this, and this class exists for exactly one
  # reason: to assert, independently of the router, that the request arrived on
  # the admin hostname.
  #
  # The host constraint in config/routes.rb already makes these routes
  # non-existent on the public host, so in normal operation this check never
  # fires. It is here because that constraint is one line in a file that will be
  # edited again — moving the namespace, adding a route outside the block, or a
  # merge that drops the `constraints` wrapper would all silently publish the
  # admin to the internet. A boundary worth having is worth enforcing in two
  # places that fail independently.
  #
  # It fails as a 404, not a 403: the public site should not disclose that an
  # admin exists at all.
  #
  # There is deliberately NO password here. The tailnet is the security
  # boundary, which is the same trade every other service in the fleet makes —
  # device authentication happens once, at the network layer. The difference
  # worth noting is that this app is also publicly served, so that trade rests
  # entirely on the host separation above rather than on the network alone.
  class BaseController < ApplicationController
    before_action :require_admin_host

    private

    def require_admin_host
      return if request.host == Rails.application.config.x.admin_host

      raise ActionController::RoutingError, "Not Found"
    end
  end
end
