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

  # The sessions list (M2) stays at /sessions; the root moved to the desk
  # (DW-002 §6). show renders the transcript (M3).
  resources :sessions, only: %i[index show create]

  # DW-002: the review. A change is addressed by repo name and card number —
  # the card number is the only identifier the user sees, the repo scopes it.
  # Flat member routes in the house style; the verbs POST to their own.
  get "changes/:repo/:card" => "changes#show", as: :change, constraints: { card: /\d+/ }
  get "changes/:repo/:card/status" => "changes#status", as: :change_status, constraints: { card: /\d+/ }
  post "changes/:repo/:card/approve" => "changes#approve", as: :approve_change, constraints: { card: /\d+/ }
  post "changes/:repo/:card/request_changes" => "changes#request_changes",
       as: :request_changes_change, constraints: { card: /\d+/ }

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

  # The model picker POSTs the chosen model to its own member route.
  post "sessions/:id/model" => "sessions#model", as: :session_model

  # DW-002 §6: one page ordered by what needs you — changes in review on
  # top, then working, then idle. The sessions list survives at /sessions.
  root "desk#index"
end
