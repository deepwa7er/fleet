require "json"
require "net/http"

# Thin, stateless HTTP client for the skiff bridge (bridge/server.js) — the
# multi-harness backend that serves pi, muse, and opencode sessions behind
# one API. Session ids on the wire are harness-qualified ("pi:…", "muse:…",
# "opencode:…"); this client passes them through verbatim.
#
# DW-001 §8 discipline: this client exists so the rest of the app never
# touches Net::HTTP or the bridge's JSON shapes directly, and so every
# failure — a bad status, a dropped connection, unparseable JSON — surfaces
# as the single BridgeClient::Error. It is stateless: every call opens its
# own connection, which is why configuration is read from ENV at call time
# rather than frozen at class load (tests override SKIFF_BRIDGE_PASSWORD
# directly). It never retries and never logs the password or request bodies.
class BridgeClient
  # Raised for every client failure. The message always starts
  # "skiff bridge unreachable: <cause>" so logs and pages can treat all
  # failures uniformly.
  class Error < StandardError; end

  USERNAME = "skiff"
  DEFAULT_BASE_URL = "http://127.0.0.1:4120"
  OPEN_TIMEOUT = 5
  READ_TIMEOUT = 15

  class << self
    def health
      get("/global/health")
    end

    # Returns { sessions: [...], errors: { harness => message } }: the merged
    # list across every harness, plus per-harness failures (an unreachable
    # opencode serve, say) so the page can show the gap instead of silently
    # dropping those sessions.
    def sessions
      get("/session")
    end

    def session(id)
      get("/session/#{id}")
    end

    # Returns the raw parsed array of { info:, parts: } for the session.
    def messages(id)
      get("/session/#{id}/message")
    end

    def prompt_async(id, text)
      post("/session/#{id}/prompt_async", { parts: [ { type: "text", text: text } ] })
    end

    # Toggle the orchestrator extension's mode for one session. pi-only: the
    # bridge rejects the toggle for any other harness, and the view never
    # renders the control without the capability.
    def orchestrator(id, on)
      post("/session/#{id}/orchestrator", { on: on })
    end

    # Set the session display name. Available where the session's harness has
    # a rename surface (capabilities.rename): pi persists a session_info
    # entry, opencode PATCHes the session; muse names its own sessions.
    def rename_session(id, name)
      post("/session/#{id}/name", { name: name })
    end

    def abort(id)
      post("/session/#{id}/abort", nil)
    end

    # The models sessions of this harness can switch to (pi only today —
    # capabilities.model). Each entry is { provider:, id: }.
    def models(harness)
      get("/harness/#{harness}/models")
    end

    # Switch one session's model. pi appends a model_change entry (the next
    # session fetch serves the new model) and keeps the choice as its
    # default for future sessions.
    def set_model(id, provider:, model:)
      post("/session/#{id}/model", { provider: provider, id: model })
    end

    def create_session(harness:, title: nil)
      post("/session", { harness: harness, title: title }.compact)
    end

    # Open the session's SSE stream and yield each body chunk as it arrives.
    # Liveness is the bridge's job — the stream stays open for the page's
    # lifetime, so this read deliberately has no read timeout (an idle
    # session would otherwise be killed mid-stream); the connection ends when
    # the bridge closes it, and any failure raises Error like every other
    # call.
    def stream_session(id)
      uri = URI.parse("#{base_url}/session/#{id}/stream")
      http = Net::HTTP.new(uri.host, uri.port)
      http.open_timeout = OPEN_TIMEOUT
      http.read_timeout = nil
      http.request(build_request(:get, uri, nil)) do |response|
        raise_for_status(response)
        response.read_body { |chunk| yield chunk }
      end
    rescue JSON::ParserError => e
      raise Error, "skiff bridge unreachable: invalid JSON response: #{e.message}"
    rescue Net::OpenTimeout, Net::ReadTimeout, Errno::ECONNREFUSED, Errno::ECONNRESET,
           SocketError, EOFError => e
      raise Error, "skiff bridge unreachable: #{e.message}"
    end

    private

    def get(path)
      request(:get, path)
    end

    def post(path, body)
      request(:post, path, body)
    end

    def request(method, path, body = nil)
      uri = URI.parse("#{base_url}#{path}")
      http = Net::HTTP.new(uri.host, uri.port)
      http.open_timeout = OPEN_TIMEOUT
      http.read_timeout = READ_TIMEOUT

      http_request = build_request(method, uri, body)
      response = http.request(http_request)
      raise_for_status(response)
      parse_body(response.body)
    rescue JSON::ParserError => e
      raise Error, "skiff bridge unreachable: invalid JSON response: #{e.message}"
    rescue Net::OpenTimeout, Net::ReadTimeout, Errno::ECONNREFUSED, Errno::ECONNRESET,
           SocketError, EOFError => e
      raise Error, "skiff bridge unreachable: #{e.message}"
    end

    def build_request(method, uri, body)
      request_class = method == :post ? Net::HTTP::Post : Net::HTTP::Get
      http_request = request_class.new(uri.request_uri)
      http_request.basic_auth(USERNAME, password)
      http_request["Accept"] = "application/json"
      if body
        http_request["Content-Type"] = "application/json"
        http_request.body = JSON.generate(body)
      end
      http_request
    end

    def raise_for_status(response)
      return if response.is_a?(Net::HTTPSuccess)

      snippet = response.body.to_s.strip[0, 120]
      detail = "HTTP #{response.code}"
      detail += ": #{snippet}" unless snippet.empty?
      raise Error, "skiff bridge unreachable: #{detail}"
    end

    # Deep-symbolized JSON; nil for empty bodies (e.g. a 204).
    def parse_body(body)
      return nil if body.nil? || body.empty?

      JSON.parse(body, symbolize_names: true)
    end

    def base_url
      ENV["SKIFF_BRIDGE_URL"] || DEFAULT_BASE_URL
    end

    def password
      ENV["SKIFF_BRIDGE_PASSWORD"].to_s
    end
  end
end
