# Persists a run bundle.
#
# The parsing half lives in Analysis::RunBundle, and the split is what lets the
# two happen on different machines: a generator parses its own results and sends
# the bundle here, so this dashboard never needs the 165MB of CSV behind it — or
# a copy of every generator's results directory.
#
# Parsing is done once at import rather than per request: the largest metrics.csv
# seen so far is 695k lines, which is several seconds of work and completely
# wasted on every page view.
#
# Importing the same run twice replaces its derived rows rather than duplicating
# them, so re-importing after an analyzer change is safe and is the normal way to
# refresh a run. That idempotence is now load-bearing rather than merely
# convenient: it is what lets two generators publish into one dashboard.
module Analysis
  class Importer
    attr_reader :directory

    def initialize(directory)
      @directory = Pathname.new(directory)
    end

    # Imports every run directory under a campfire-stress results/ folder.
    #
    # Only useful on a machine that has one — which, since publishing became a
    # per-run push, is the generator rather than the dashboard.
    def self.import_all(results_root)
      root = Pathname.new(results_root)
      return [] unless root.directory?

      root.children
          .select(&:directory?)
          .select { |dir| dir.join(RunBundle::METRICS_FILE).exist? }
          .sort
          .map { |dir| new(dir).import }
    end

    def import = self.class.apply(RunBundle.build(directory))

    # Writes a bundle into the database, replacing whatever was there for that
    # run.
    def self.apply(bundle)
      run = Run.find_or_initialize_by(stamp: bundle.stamp)

      Run.transaction do
        attributes = bundle.run.slice(*RUN_ATTRIBUTES)

        run.assign_attributes(
          **attributes,
          started_at: timestamp(bundle.run["started_at"]),
          imported_at: Time.current,
          # A text column holding JSON, so the settings a page lists survive as
          # one field rather than a table nothing else joins against.
          config: bundle.run["config"].to_json
        )
        run.save!

        # Derived rows are rebuilt wholesale. Updating in place would leave
        # stale levels behind whenever the segmentation rules change.
        run.level_stats.delete_all
        run.server_samples.delete_all
        run.throughput_samples.delete_all
        RunProgress.where(run: run).delete_all

        persist_levels(run, bundle.levels)
        persist_samples(ServerSample, run, bundle.server_samples, SERVER_SAMPLE_ATTRIBUTES)
        persist_samples(ThroughputSample, run, bundle.throughput_samples, THROUGHPUT_SAMPLE_ATTRIBUTES)
        RunProgress.create!(run: run, payload: bundle.progress) if bundle.progress
      end

      # The sample tables are written with insert_all, which goes straight to the
      # database and past the associations this method emptied a moment earlier.
      # Without resetting them, the run handed back reports no samples at all
      # while the rows sit there in the table.
      run.level_stats.reset
      run.server_samples.reset
      run.throughput_samples.reset
      run.reload_progress

      run
    end

    # Assigned by name rather than splatted wholesale: a bundle arrives over the
    # network, and assign_attributes on unfiltered input would let a sender write
    # any column on this table.
    RUN_ATTRIBUTES = %w[
      path scenario target generator machine k6_version
      variant server_image server_digest server_env
      peak_cpu_pct peak_wal_bytes
      total_broadcasts total_requests unanswered_requests harness_errors
    ].freeze

    LEVEL_ATTRIBUTES = (%w[
      vus window_seconds sample_count requests_per_second megabytes_per_second
      error_count cpu_avg_pct cpu_peak_pct
    ] + LATENCY_METRICS.values.map(&:to_s)).freeze

    SERVER_SAMPLE_ATTRIBUTES = %w[cpu_pct mem_pct load1 wal_bytes db_bytes].freeze
    THROUGHPUT_SAMPLE_ATTRIBUTES = %w[requests].freeze

    # Epoch seconds on the wire, because a bundle crosses a machine boundary and
    # two boxes need not agree on a time zone.
    def self.timestamp(epoch) = epoch && Time.zone.at(epoch)
    private_class_method :timestamp

    def self.persist_levels(run, levels)
      levels.each { |level| run.level_stats.create!(level.slice(*LEVEL_ATTRIBUTES)) }
    end
    private_class_method :persist_levels

    # insert_all rather than create!, because a run holds one row per second of
    # its own length in each of these tables and the difference is thousands of
    # statements.
    def self.persist_samples(model, run, samples, attributes)
      now = Time.current

      rows = samples.map do |sample|
        sample.slice(*attributes).merge(
          "run_id" => run.id,
          "at" => timestamp(sample["at"]),
          "created_at" => now,
          "updated_at" => now
        )
      end

      model.insert_all(rows) if rows.any?
    end
    private_class_method :persist_samples
  end
end
