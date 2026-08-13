# Re-buckets a run's samples by the VU count in effect when each was recorded.
#
# This is the whole point of the dashboard. k6's end-of-run summary aggregates
# every ramp step into one set of percentiles, so a p95 spanning 25 and 800 VUs
# describes neither, and the knee -- the level where the app stops keeping up --
# is averaged away.
#
# Levels come from the `vus` gauge rather than the configured schedule, so the
# output reflects what actually ran.
module Analysis
  class LevelBreakdown
    Segment = Struct.new(:vus, :started_at, :ended_at) do
      def duration = ended_at - started_at
    end

    Level = Struct.new(
      :vus, :window_seconds, :sample_count, :request_count,
      :latencies, :requests_per_second, :megabytes_per_second,
      :error_count, :cpu_avg_pct, :cpu_peak_pct,
      keyword_init: true
    ) do
      # A plateau nothing was asked of.
      #
      # Every VU-based scenario ends with `gracefulRampDown` and `gracefulStop`,
      # so a handful of stragglers finishing their last session can sit at a
      # constant two or three VUs for well over the minimum hold — long enough to
      # look exactly like a level. It is not one: no request completed during it.
      #
      # Left in, it becomes a point at (2 people, 0 req/s) that the chart draws a
      # line up from, turning the dying tail of a run into an apparent load
      # curve, and it feeds the same fiction to #knee and #breaking_point.
      #
      # A stalled server is not caught by this: k6 records an http_reqs sample
      # for a request that failed or timed out just as it does for one that
      # succeeded, so a level under a server that has stopped answering still
      # counts requests. Zero here means nothing was even being attempted.
      def idle? = request_count.to_f.zero?
    end

    def initialize(metrics, server_trace = nil)
      @metrics = metrics
      @server_trace = server_trace
    end

    # Contiguous runs of a constant VU count.
    #
    # Contiguity is not a detail: a ramp visits the same level twice, once
    # climbing and once on the way back down. Treating those as a single window
    # would merge two unrelated periods and count the gap between them as
    # elapsed time, silently deflating throughput.
    def segments
      @segments ||= begin
        result = []
        current = nil
        started_at = nil
        previous_at = nil

        @metrics.vu_timeline.each do |at, vus|
          if current.nil? || vus != current
            result << Segment.new(current, started_at, previous_at) unless current.nil?
            current = vus
            started_at = at
          end
          previous_at = at
        end

        result << Segment.new(current, started_at, previous_at) unless current.nil?
        result
      end
    end

    # The levels worth reporting: those held long enough to be a plateau.
    def plateau_levels
      segments
        .select { |s| s.vus.positive? && s.duration >= MIN_HOLD_SECONDS }
        .map(&:vus)
        .uniq
        .sort
    end

    # The levels the run actually held under load. See Level#idle? for why a
    # plateau with no traffic in it is dropped rather than reported.
    def levels
      plateau_levels.filter_map { |vus| level_for(vus) }.reject(&:idle?)
    end

    private

    # The longest contiguous hold at this level, with the settling portion of
    # the window trimmed off the front.
    def window_for(vus)
      held = segments.select { |s| s.vus == vus && s.duration >= MIN_HOLD_SECONDS }
      return nil if held.empty?

      longest = held.max_by(&:duration)
      settle = longest.duration * SETTLE_FRACTION
      (longest.started_at + settle)..longest.ended_at
    end

    def level_for(vus)
      window = window_for(vus)
      return nil if window.nil?

      span = [ window.last - window.first, 1 ].max

      latencies = {}
      sample_count = 0
      LATENCY_METRICS.each do |metric, attribute|
        values = @metrics.latency_samples[metric]
                         .select { |s| window.cover?(s.at) }
                         .map(&:value)
        sample_count += values.length
        latencies[attribute] = Percentile.of(values, 95)
      end

      requests = counter_sum("http_reqs", window)

      Level.new(
        vus: vus,
        window_seconds: span,
        sample_count: sample_count,
        request_count: requests,
        latencies: latencies,
        requests_per_second: requests / span,
        megabytes_per_second: counter_sum("data_received", window) / span / 1_048_576.0,
        error_count: counter_sum("campfire_errors", window).round,
        cpu_avg_pct: @server_trace&.average_cpu(window),
        cpu_peak_pct: @server_trace&.peak_cpu(window)
      )
    end

    def counter_sum(metric, window)
      @metrics.counter_samples[metric]
              .select { |s| window.cover?(s.at) }
              .sum(&:value)
    end
  end
end
