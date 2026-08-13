Rails.application.routes.draw do
  root "runs#index"

  # One address per run, live or finished: /runs/<stamp>.
  #
  # By stamp rather than by database id, because a run in flight has no row yet
  # — rows appear at import — while the stamp already names the same run to the
  # runner, to its results directory and to this database, and is stable across
  # instances in a way ids are not.
  resources :runs, only: %i[ index show ], param: :stamp do
    member do
      # The series behind this run's charts: live from the runner while it is in
      # flight, stored afterwards. Separate from the page because the chart
      # draws onto a canvas rather than being re-rendered as HTML.
      get :progress

      # State and the stop button, while the runner still knows this run.
      # Polled, so it is a fragment rather than part of the page.
      get :status

      # The running request totals, which climb while a run is in flight.
      get :requests

      # The charts. A fragment because a live run may not have a series to draw
      # yet, and that changes while its page is open.
      get :charts
      post :cancel
      post :publish
    end

    collection do
      # Rescanning the results directory is a state change, so it is a POST
      # rather than a link.
      post :import
      get :compare
      post :archive
      post :restore
    end
  end

  # Launching a load test, and only that. Once a run exists it is a run like any
  # other and lives at /runs/<stamp>, whether it is still going or long over.
  # Available only where a runner is reachable; the deployed instance redirects
  # away from this.
  resources :test_runs, only: %i[ new create ], path: "tests"

  # Which build of Campfire the next test measures. A fragment rather than a
  # page: it lives inside the new-test form and is polled while a switch runs.
  get "variant" => "variants#show", as: :variant
  post "variant/:name/switch" => "variants#switch", as: :switch_variant

  # How finished runs reach this dashboard.
  #
  # The raw results are ~165MB of CSV on the machine that generated the load, and
  # this app is not on that machine — so the generator parses its own results and
  # posts the parsed bundle here. Machine-to-machine, so it is authenticated by a
  # shared token rather than by a session. See Api::RunsController.
  namespace :api do
    resources :runs, only: :create
  end

  # Returns 200 if the app boots with no exceptions.
  get "up" => "rails/health#show", as: :rails_health_check
end
