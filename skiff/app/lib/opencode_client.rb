require "json"
require "net/http"

# Thin, stateless HTTP client for the headless opencode server.
#
# DW-001 §8 discipline: this client exists so the rest of the app never touches
# Net::HTTP or the server's JSON shapes directly, and so every failure — a bad
# status, a dropped connection, unparseable JSON — surfaces as the single
# OpencodeClient::Error. It is stateless: every call opens its own connection,
# which is why configuration is read from ENV at call time rather than frozen
# at class load (tests override OPENCODE_SERVER_PASSWORD directly). It never
# retries and never logs the password or request bodies.
class OpencodeClient
  # Raised for every client failure. The message always starts
  # "opencode server unreachable: <cause>" so logs and pages can treat all
  # failures uniformly.
  class Error < StandardError; end

  USERNAME = "opencode"
  DEFAULT_BASE_URL = "http://127.0.0.1:4120"
  OPEN_TIMEOUT = 5
  READ_TIMEOUT = 15

  class << self
    def health
      get("/global/health")
    end

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

    # Toggle the orchestrator extension's mode for one session. This endpoint
    # is bridge-specific (not part of the opencode contract): the bridge
    # drives the session's live pi process through its /orchestrator command,
    # and the extension persists the mode into the session file.
    def orchestrator(id, on)
      post("/session/#{id}/orchestrator", { on: on })
    end

    # Set the session display name. Bridge-specific (like orchestrator): the
    # bridge drives the session's live pi process through its set_session_name
    # command, and pi persists the name as a session_info entry in the
    # session file, so the next session fetch serves the new title.
    def rename_session(id, name)
      post("/session/#{id}/name", { name: name })
    end

    def abort(id)
      post("/session/#{id}/abort", nil)
    end

    def create_session(title: nil)
      post("/session", { title: title }.compact)
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
      raise Error, "opencode server unreachable: invalid JSON response: #{e.message}"
    rescue Net::OpenTimeout, Net::ReadTimeout, Errno::ECONNREFUSED, Errno::ECONNRESET,
           SocketError, EOFError => e
      raise Error, "opencode server unreachable: #{e.message}"
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
      raise Error, "opencode server unreachable: invalid JSON response: #{e.message}"
    rescue Net::OpenTimeout, Net::ReadTimeout, Errno::ECONNREFUSED, Errno::ECONNRESET,
           SocketError, EOFError => e
      raise Error, "opencode server unreachable: #{e.message}"
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
      raise Error, "opencode server unreachable: #{detail}"
    end

    # Deep-symbolized JSON; nil for empty bodies (e.g. a 204).
    def parse_body(body)
      return nil if body.nil? || body.empty?

      JSON.parse(body, symbolize_names: true)
    end

    def base_url
      ENV["OPENCODE_SERVER_URL"] || DEFAULT_BASE_URL
    end

    def password
      ENV["OPENCODE_SERVER_PASSWORD"].to_s
    end
  end
end
