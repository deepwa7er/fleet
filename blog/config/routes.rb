Rails.application.routes.draw do
  # ── Public: readable from the internet ────────────────────────────────────
  root "posts#index"
  get "/posts/:id", to: "posts#show", as: :post
  get "/feed", to: "feeds#show", as: :feed, defaults: { format: :atom }

  # Rails' health endpoint: 200 only if the app boots cleanly. tugboat's deploy
  # health check curls this on the host loopback, so a build that loads but
  # cannot boot fails the deploy and rolls back.
  get "up" => "rails/health#show", as: :rails_health_check

  # ── Admin: the tailnet only ───────────────────────────────────────────────
  #
  # Drawn inside a host constraint, so these routes DO NOT EXIST for a request
  # arriving on the public hostname — the router raises RoutingError and Rails
  # renders 404 before any controller is reached. See config/application.rb for
  # why the Host header is trustworthy here, and Admin::BaseController for the
  # second, independent check.
  constraints host: Rails.application.config.x.admin_host do
    namespace :admin do
      root "posts#index"
      resources :posts do
        member do
          post :publish
          post :unpublish
        end
      end
    end
  end
end
