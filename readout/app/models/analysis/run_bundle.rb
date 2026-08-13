# Everything worth keeping about one run, as plain data.
#
# This is the seam between parsing and storing. A results directory is ~165MB of
# CSV and JSON on the machine that generated the load; what survives it is the
# few hundred kilobytes below. Naming that boundary is what lets the two happen
# in different places — parsed on the generator, stored on the dashboard — which
# is the whole reason load can now come from more than one machine.
#
# Deliberately stdlib-only, like the rest of Analysis::*. bin/publish requires
# these files directly on a generator box and never boots Rails: a load
# generator should not need a Rails app and a bundle of gems installed on it just
# to describe what it measured.
#
# Timestamps are epoch seconds rather than formatted strings. They cross a
# machine boundary here, and two boxes need not agree on a time zone.
require "json"
require "pathname"
require "set"

module Analysis
  class RunBundle
    # Bumped when the shape below changes incompatibly. The receiving end
    # refuses a version it does not know rather than silently importing a run
    # with fields it cannot read — a wrong number on this dashboard is worse
    # than a missing one, because nothing about it looks wrong.
    FORMAT = 1

    class Invalid < StandardError; end

    # No metrics.csv: the directory is not a run at all, or is one that died
    # before k6 wrote anything.
    class MissingMetrics < Invalid; end

    # progress.json exists but is not JSON. Its own class because it means
    # something quite different from an unparseable directory: the run happened
    # and its charts are the part that is broken.
    class MalformedProgress < Invalid; end

    METRICS_FILE  = "metrics.csv"
    SERVER_FILE   = "server.csv"
    CONFIG_FILE   = "run-config.txt"
    PROGRESS_FILE = "progress.json"

    # A run's directory name, which is also its identity everywhere: the results
    # folder, the runner's id, the URL, and the unique key in the database.
    # Checked because it arrives over the network and is used as all four.
    STAMP = /\A\d{8}-\d{6}\z/

    attr_reader :stamp, :run, :levels, :server_samples, :throughput_samples, :progress

    def initialize(stamp:, run:, levels:, server_samples:, throughput_samples:, progress:)
      @stamp = stamp
      @run = run
      @levels = levels
      @server_samples = server_samples
      @throughput_samples = throughput_samples
      @progress = progress
    end

    # Reads a campfire-stress results directory.
    def self.build(directory)
      directory = Pathname.new(directory)
      stamp = directory.basename.to_s

      metrics_path = directory.join(METRICS_FILE)
      raise MissingMetrics, "no #{METRICS_FILE} in #{directory}" unless metrics_path.exist?

      metrics = MetricsFile.parse(metrics_path.to_s)
      trace = ServerTrace.parse(directory.join(SERVER_FILE).to_s)
      config = RunConfig.parse(directory.join(CONFIG_FILE).to_s)
      breakdown = LevelBreakdown.new(metrics, trace)
      window = run_window(metrics)

      new(
        stamp: stamp,
        run: {
          # Where the raw results are, on the machine named below. Meaningless
          # as a path on the dashboard, and kept anyway: with more than one
          # generator, "which box, which directory" is the only way back to the
          # CSV behind a number.
          "path" => directory.to_s,
          "scenario" => config.scenario,
          "target" => config.target,
          "generator" => config.generator,
          "machine" => config.machine,
          "k6_version" => config.k6_version,
          "variant" => config.variant,
          "server_image" => config.server_image,
          "server_digest" => config.server_digest,
          "server_env" => config.server_env,
          "started_at" => window&.first,
          "peak_cpu_pct" => window && trace.peak_cpu(window),
          "peak_wal_bytes" => window && trace.peak_wal_bytes(window),
          "total_broadcasts" => total_of(metrics, "campfire_broadcasts_received"),
          "total_requests" => total_of(metrics, "http_reqs"),
          "unanswered_requests" => metrics.unanswered_requests,
          "harness_errors" => total_of(metrics, "campfire_errors"),
          "config" => config.explicit_settings
        },
        levels: breakdown.levels.map { |level| level_row(level) },
        server_samples: trace_rows(trace, window),
        throughput_samples: throughput_rows(metrics, window),
        progress: progress_payload(directory.join(PROGRESS_FILE))
      )
    end

    # Rebuilds a bundle from the wire, checking what it is before anything reads
    # it as a run.
    def self.from_h(raw)
      raise Invalid, "expected a JSON object" unless raw.is_a?(Hash)

      format = raw["format"]
      raise Invalid, "unsupported bundle format #{format.inspect}; this dashboard reads #{FORMAT}" unless format == FORMAT

      stamp = raw["stamp"].to_s
      raise Invalid, "#{stamp.inspect} is not a run stamp" unless stamp.match?(STAMP)

      run = raw["run"]
      raise Invalid, "bundle has no run" unless run.is_a?(Hash)

      new(
        stamp: stamp,
        run: run,
        levels: array(raw, "levels"),
        server_samples: array(raw, "server_samples"),
        throughput_samples: array(raw, "throughput_samples"),
        # A run may genuinely have no chart payload — anything that finished
        # before the runner wrote progress.json — so absent is a valid state
        # rather than an error. Present-but-not-an-object is not.
        progress: raw["progress"].is_a?(Hash) ? raw["progress"] : nil
      )
    end

    # Bad JSON is a bad bundle, not a different kind of problem: a publisher
    # that sent a truncated body needs the same answer as one that sent the
    # wrong shape — this was refused, send it again.
    def self.from_json(text)
      from_h(JSON.parse(text.to_s))
    rescue JSON::ParserError => e
      raise Invalid, "not valid JSON: #{e.message}"
    end

    def to_h
      {
        "format" => FORMAT,
        "stamp" => stamp,
        "run" => run,
        "levels" => levels,
        "server_samples" => server_samples,
        "throughput_samples" => throughput_samples,
        "progress" => progress
      }
    end

    def to_json(*args) = to_h.to_json(*args)

    # Which machine generated this run's load, for a caller reporting what it is
    # about to send. Nil for runs from before bin/run.sh recorded it.
    def machine = run["machine"]

    def self.array(raw, key)
      value = raw[key]
      return [] if value.nil?
      raise Invalid, "#{key} must be a list" unless value.is_a?(Array)

      value
    end
    private_class_method :array

    # The span k6 was actually generating load over, used to bound every
    # server-side figure so foreign samples in the file cannot leak in.
    def self.run_window(metrics)
      first = metrics.vu_timeline.first&.first
      last = metrics.vu_timeline.last&.first
      return nil if first.nil? || last.nil?

      first..last
    end
    private_class_method :run_window

    def self.total_of(metrics, name)
      metrics.counter_samples[name].sum(&:value).round
    end
    private_class_method :total_of

    def self.level_row(level)
      {
        "vus" => level.vus,
        "window_seconds" => level.window_seconds,
        "sample_count" => level.sample_count,
        "requests_per_second" => level.requests_per_second,
        "megabytes_per_second" => level.megabytes_per_second,
        "error_count" => level.error_count,
        "cpu_avg_pct" => level.cpu_avg_pct,
        "cpu_peak_pct" => level.cpu_peak_pct
      }.merge(level.latencies.transform_keys(&:to_s))
    end
    private_class_method :level_row

    def self.trace_rows(trace, window)
      readings = window ? trace.within(window) : trace.readings

      readings.map do |reading|
        {
          "at" => reading.at,
          "cpu_pct" => reading.cpu_pct,
          "mem_pct" => reading.mem_pct,
          "load1" => reading.load1,
          "wal_bytes" => reading.wal_bytes,
          "db_bytes" => reading.db_bytes
        }
      end
    end
    private_class_method :trace_rows

    # Requests completed per second, for the whole run.
    #
    # This is what a single-level run can be charted against: chat and
    # enterprise runs hold one population from start to finish, so throughput
    # against load is a single point and only throughput against time has a
    # shape. k6 stamps every sample to the whole second, which is exactly the
    # resolution wanted here, so the series is a tally rather than a rolling
    # window.
    #
    # Every second in the run's window gets an entry, including the ones nothing
    # completed in. See ThroughputSample for why a zero is not the same as a gap.
    def self.throughput_rows(metrics, window)
      return [] if window.nil?

      completed = Hash.new(0.0)
      metrics.counter_samples["http_reqs"].each do |sample|
        completed[sample.at] += sample.value if window.cover?(sample.at)
      end

      window.map { |second| { "at" => second, "requests" => completed[second].round } }
    end
    private_class_method :throughput_rows

    # The series the run's charts are drawn from, taken whole from the runner.
    #
    # Not computed here, and deliberately: they come from k6's JSON output at
    # 33ms resolution, and the runner has already computed them once to feed the
    # live chart. Parsing that file a second time in another language would put
    # two implementations of the same arithmetic in the same product, which is
    # exactly how the two views came to disagree. See RunProgress.
    #
    # Absent for runs that finished before the runner wrote this file. Those
    # pages say so rather than drawing an empty chart; `bin/runner
    # --rebuild-progress`, on the machine holding results/, is what fills them in.
    def self.progress_payload(path)
      return nil unless path.exist?

      JSON.parse(path.read)
    rescue JSON::ParserError => e
      # Loud rather than skipped. The file is written atomically, so a
      # half-written one means something is wrong with the run's output, and a
      # page silently missing its charts is the failure this whole path exists
      # to remove.
      raise MalformedProgress, "#{path} is not valid JSON: #{e.message}"
    end
    private_class_method :progress_payload
  end
end
