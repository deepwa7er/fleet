Rails.application.routes.draw do
  # Define your application routes per the DSL in https://guides.rubyonrails.org/routing.html

  # Reveal health status on /up that returns 200 if the app boots with no exceptions, otherwise 500.
  # Can be used by load balancers and uptime monitors to verify that the app is live.
  get "up" => "rails/health#show", as: :rails_health_check

  # Render dynamic PWA files from app/views/pwa/*. The service worker must be
  # served from the scope root (it is) with no redirects — breakwater forwards
  # it verbatim, like every other path.
  get "manifest" => "rails/pwa#manifest", as: :pwa_manifest
  get "service-worker" => "rails/pwa#service_worker", as: :pwa_service_worker

  # The sessions list is the root page (M2). show renders the transcript (M3).
  resources :sessions, only: %i[index show create]

  # M3: the composer posts to the session's messages. A plain member route
  # rather than `resources :sessions { post :messages }`, because the action
  # lives in its own controller; session_messages_path(id) reads naturally at
  # the call site.
  post "sessions/:id/messages" => "messages#create", as: :session_messages

  # M4: the session view streams over SSE. The browser opens one EventSource
  # per page (sessions#stream proxies the bridge's stream, translating its
  # events into turbo-streams); the composer POSTs to its own route, and the
  # abort/orchestrator/rename keys POST to their own.
  get "sessions/:id/stream" => "sessions#stream", as: :session_stream
  post "sessions/:id/abort" => "sessions#abort", as: :abort_session
  post "sessions/:id/orchestrator" => "sessions#orchestrator", as: :session_orchestrator

  # The rename key POSTs to its own member route, like abort and the
  # orchestrator toggle.
  post "sessions/:id/name" => "sessions#rename", as: :rename_session

  root "sessions#index"
end
