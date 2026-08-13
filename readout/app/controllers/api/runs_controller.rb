# Accepts a finished run from the machine that generated it.
#
# This is how results reach the dashboard. The raw output of a run is ~165MB of
# CSV and JSON on the generator's disk, and this app is not on that machine — so
# the generator parses its own results and pushes the few hundred kilobytes worth
# keeping. See Analysis::RunBundle.
#
# It replaced shipping the whole SQLite database up by rsync, which worked only
# while exactly one machine generated load. Two machines each hold a database
# containing only their own runs, so whichever published last replaced the
# other's history. Per-run and idempotent is what makes more than one generator
# possible at all.
module Api
  class RunsController < ActionController::API
    # A bundle is normally a few hundred kilobytes; the chart payload is the
    # bulk of it. This ceiling is far above any real run and exists so a
    # malformed or hostile body cannot be parsed into memory on a 2GB VPS where
    # this app is capped at 400MB.
    MAX_BUNDLE_BYTES = 32.megabytes

    before_action :require_token
    before_action :require_reasonable_size

    def create
      bundle = Analysis::RunBundle.from_json(request.raw_post)
      run = Analysis::Importer.apply(bundle)

      render json: {
        stamp: run.stamp,
        machine: run.machine,
        levels: run.level_stats.length,
        charted: run.progress.present?
      }, status: :created
    rescue Analysis::RunBundle::Invalid => e
      # The sender's fault and worth saying precisely: a publish that fails must
      # name what was wrong with the bundle, because the results still exist on
      # the generator and the fix is to send them again.
      render json: { error: e.message }, status: :unprocessable_content
    end

    private

    def require_token
      unless IngestToken.configured?
        render json: { error: "this dashboard accepts no published runs" },
               status: :service_unavailable
        return
      end

      return if IngestToken.matches?(request.headers["X-Readout-Token"])

      render json: { error: "missing or invalid X-Readout-Token" }, status: :unauthorized
    end

    def require_reasonable_size
      length = request.content_length.to_i
      return if length <= MAX_BUNDLE_BYTES

      render json: { error: "bundle is #{length} bytes; the limit is #{MAX_BUNDLE_BYTES}" },
             status: :payload_too_large
    end
  end
end
