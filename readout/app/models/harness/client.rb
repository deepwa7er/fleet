require "net/http"
require "json"

# Talks to the local runner service (campfire-stress/runner), which launches load
# tests on the machine that actually has k6.
#
# Every method fails soft: the runner is optional infrastructure, and the
# deployed instance has none. A dashboard that raised because a helper service
# was absent would be worse than one that simply does not offer launching.
module Harness
  class Client
    class Error < StandardError; end
    class Busy < Error; end

    # Deliberately short. These calls sit in the request path, and a hung runner
    # must not hang the page — the dashboard's read-only half has to keep working
    # regardless.
    #
    # Five seconds is thin for #variants, which crosses a network, a login shell
    # and the Docker daemon before it answers. That is a known, accepted
    # trade-off rather than an oversight: see the note in VariantsController.
    OPEN_TIMEOUT = 1
    READ_TIMEOUT = 5

    # Addressed explicitly rather than read from configuration, because there is
    # more than one runner now and a client that defaulted to "the" runner would
    # be picking one of several without being asked to. Harness::Runner#client is
    # how one is built.
    def initialize(url:, token_file: nil)
      @url = URI.parse(url)
      @token_file = token_file
    end

    # True when a runner is reachable. Used to decide whether to offer launching
    # at all, so this must never raise.
    def available?
      health.present?
    rescue Error
      false
    end

    def health
      get("/healthz")
    rescue Error
      nil
    end

    def runs
      get("/runs")&.fetch("runs", []) || []
    end

    def run(id)
      get("/runs/#{CGI.escape(id)}")
    end

    # k6's output is UTF-8 (box drawing, check marks), but Net::HTTP hands back
    # ASCII-8BIT, which blows up on render against a UTF-8 template. It is also
    # a *tail*: the runner seeks to a byte offset, so the first character can be
    # half of a multi-byte sequence. Scrubbing handles both — re-tagging alone
    # would leave an invalid byte to fail later, further from the cause.
    # Live "is the server coping" series while a run is in flight. Returns nil
    # rather than raising: a missing chart must not take the status page down.
    def progress(id)
      get("/runs/#{CGI.escape(id)}/progress")
    rescue Error
      nil
    end

    def log(id)
      body = request(Net::HTTP::Get.new(path_for("/runs/#{CGI.escape(id)}/log")), parse: false)
      body.to_s.dup.force_encoding(Encoding::UTF_8).scrub("")
    end

    # Which builds of Campfire may be deployed, which one is deployed now, and
    # whether a switch is under way. See campfire-stress/variants.json.
    def variants
      get("/variants")
    end

    # Deploys one of those builds. Returns as soon as the switch has started —
    # it restarts a container and may build an image first, which is far longer
    # than a request should hold, so the caller polls #variants for the outcome.
    def switch_variant(name)
      post("/variants/#{CGI.escape(name)}/switch", nil)
    end

    def start(scenario:, levers: {}, note: nil)
      body = { scenario: scenario, levers: levers.compact_blank, note: note }
      post("/runs", body)
    end

    def cancel(id)
      post("/runs/#{CGI.escape(id)}/cancel", nil)
    end

    # Asks the runner to ship a finished run's results to this dashboard.
    #
    # The dashboard cannot import for itself when it is not on the machine that
    # ran the test: the results are CSV on the runner's disk. Runs publish
    # automatically on completion; this is the retry.
    def publish(id)
      post("/runs/#{CGI.escape(id)}/publish", nil)
    end

    private

    def token
      return @token if defined?(@token)

      @token = @token_file.present? ? File.read(@token_file).strip.presence : nil
    rescue SystemCallError
      # No token file means this instance was not given the secret for that
      # machine, which is the normal state for a dashboard with no harness
      # beside it.
      @token = nil
    end

    def path_for(path) = path

    def get(path)
      request(Net::HTTP::Get.new(path_for(path)))
    end

    def post(path, body)
      req = Net::HTTP::Post.new(path_for(path))
      req["Content-Type"] = "application/json"
      req.body = body.nil? ? "" : JSON.generate(body)
      request(req)
    end

    def request(req, parse: true)
      raise Error, "no runner token available" if token.nil?

      req["X-Runner-Token"] = token

      response = Net::HTTP.start(
        @url.host, @url.port,
        open_timeout: OPEN_TIMEOUT, read_timeout: READ_TIMEOUT
      ) { |http| http.request(req) }

      case response
      when Net::HTTPSuccess
        parse ? JSON.parse(response.body.presence || "{}") : response.body
      when Net::HTTPConflict
        raise Busy, error_message(response)
      else
        raise Error, error_message(response)
      end
    rescue JSON::ParserError => e
      raise Error, "runner returned malformed JSON: #{e.message}"
    rescue SystemCallError, Net::OpenTimeout, Net::ReadTimeout, IOError => e
      # Connection refused is the ordinary "runner isn't running" case.
      raise Error, "runner unreachable at #{@url}: #{e.class}"
    end

    def error_message(response)
      JSON.parse(response.body)["error"].presence || "runner returned #{response.code}"
    rescue JSON::ParserError
      "runner returned #{response.code}"
    end
  end
end
