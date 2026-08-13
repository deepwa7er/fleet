require "test_helper"
require "tmpdir"

module Analysis
  # End-to-end coverage of the real parsing path, against a results directory
  # built on the fly. Written rather than committed as fixture files so the
  # shape of the input is visible right here -- and so the test never depends on
  # an 80MB artifact.
  class ImporterTest < ActiveSupport::TestCase
    include ResultsDirectory

    test "imports a results directory into levels and samples" do
      Dir.mktmpdir do |dir|
        write_run(dir, stamp: "20260101-000000")
        run = Importer.new(File.join(dir, "20260101-000000")).import

        assert_equal "20260101-000000", run.stamp
        assert_equal "scenarios/ramp.js", run.scenario
        assert_equal "http://example.test", run.target
        assert_equal [ 10, 20 ], run.level_stats.pluck(:vus)

        slow = run.level_stats.find_by(vus: 20)
        assert_equal 200.0, slow.room_open_p95
        assert_in_delta 1.0, slow.requests_per_second, 0.05
        assert_in_delta 1.0, slow.megabytes_per_second, 0.05
      end
    end

    # The series a single-level run is charted against. It has to cover the whole
    # run, second by second, or the chart it feeds is the empty frame this exists
    # to replace.
    test "records requests completed per second across the whole run" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000005")
        run = Importer.new(path).import

        assert_equal 160, run.throughput_samples.count, "one row per second of the run"
        assert_equal [ 1 ], run.throughput_samples.pluck(:requests).uniq
      end
    end

    # A second in which nothing completed is a measurement, not a missing one.
    test "seconds with no completed requests are recorded as zero" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000006")

        # A ten-second stall in the middle, with the VU timeline unbroken so the
        # run's window still spans it.
        metrics = File.join(path, "metrics.csv")
        kept = File.readlines(metrics).reject do |line|
          line.start_with?("http_reqs,") && (1_700_000_070..1_700_000_079).cover?(line.split(",")[1].to_i)
        end
        File.write(metrics, kept.join)

        run = Importer.new(path).import
        stalled = run.throughput_samples.where(at: Time.zone.at(1_700_000_070)..Time.zone.at(1_700_000_079))

        assert_equal 10, stalled.count
        assert_equal [ 0 ], stalled.pluck(:requests).uniq
      end
    end

    # Stragglers finishing their sessions after a run ends hold a constant, tiny
    # VU count for long enough to look like a plateau. It is not a level, and
    # letting it become one draws a load curve out of the run's dying tail.
    test "an idle tail is not imported as a level" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000007")

        # 40 seconds at 2 VUs with no requests, appended after the run proper.
        File.open(File.join(path, "metrics.csv"), "a") do |file|
          (0..39).each { |offset| file.puts "vus,#{1_700_000_160 + offset},2,,,,,,,,,,,,,,,," }
        end

        run = Importer.new(path).import

        assert_equal [ 10, 20 ], run.level_stats.pluck(:vus)
      end
    end

    # The series behind the run's charts, taken whole from the runner. They
    # cannot be recomputed here — k6's sub-second output is 40-150MB per run and
    # never leaves the machine that generated the load — so an import that
    # dropped this file would leave the finished run's page unable to draw what
    # its own live page had just drawn.
    test "stores the runner's progress payload" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000008")
        File.write(File.join(path, "progress.json"),
          '{"deliveries":[{"t":1,"v":40}],"duration_s":60,"dropped":0}')

        run = Importer.new(path).import

        assert_equal 60, run.progress.payload["duration_s"]
        assert_equal [ { "t" => 1, "v" => 40 } ], run.progress.payload["deliveries"]
      end
    end

    test "re-importing replaces the progress payload rather than duplicating it" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000009")
        File.write(File.join(path, "progress.json"), '{"duration_s":60}')
        Importer.new(path).import

        # A rebuilt payload — the arithmetic behind these series changed, and
        # re-importing is how every run picks that up.
        File.write(File.join(path, "progress.json"), '{"duration_s":165}')
        run = Importer.new(path).import

        assert_equal 1, RunProgress.where(run: run).count
        assert_equal 165, run.progress.payload["duration_s"]
      end
    end

    # Runs that finished before the runner wrote this file simply have none, and
    # that must not stop them importing: everything else about them is still
    # good, and their page says the charts are missing in words.
    test "a run without a progress payload still imports" do
      Dir.mktmpdir do |dir|
        run = Importer.new(write_run(dir, stamp: "20260101-000010")).import

        assert_nil run.progress
        assert_equal 160, run.throughput_samples.count
      end
    end

    # Written atomically by the runner, so a half-written one means something is
    # wrong with the output rather than with the timing of this read.
    test "a malformed progress payload is reported rather than skipped" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000011")
        File.write(File.join(path, "progress.json"), '{"deliveries":[{"t":1,')

        assert_raises(RunBundle::MalformedProgress) { Importer.new(path).import }
      end
    end

    # What the load was applied to. A run that cannot name its server cannot be
    # compared against one that can, so this is imported as data rather than
    # left to be reconstructed from container timestamps afterwards.
    test "records the server variant the run measured" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000012")
        File.open(File.join(path, "run-config.txt"), "a") do |file|
          file.puts "variant=tuned"
          file.puts "server_image=localhost:5000/campfire:6d2cf0a"
          file.puts "server_digest=sha256:b31e9f2a"
          file.puts "server_env=CAMPFIRE_BATCH_UNREAD=1,RAILS_ENV=performance"
        end

        run = Importer.new(path).import

        assert_equal "tuned", run.variant
        assert run.variant_known?
        assert_equal "localhost:5000/campfire:6d2cf0a", run.server_image
        assert_equal "CAMPFIRE_BATCH_UNREAD=1,RAILS_ENV=performance", run.server_env
      end
    end

    # The server description is not a lever. Listing it among the settings would
    # read as something that was asked of the load.
    test "server fields do not leak into the run's settings" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000013")
        File.open(File.join(path, "run-config.txt"), "a") do |file|
          file.puts "variant=stock"
          file.puts "server_env=RAILS_ENV=performance"
        end

        run = Importer.new(path).import

        assert_equal({ "USER_POOL" => "800" }, run.settings)
      end
    end

    # Two states that must not collapse into each other: a run from before this
    # was recorded, and one where the harness looked and could not tell.
    test "a run that could not identify its server is not treated as one that can" do
      Dir.mktmpdir do |dir|
        old = Importer.new(write_run(dir, stamp: "20260101-000014")).import
        assert_nil old.variant
        assert_not old.variant_known?
        assert_equal "server not recorded", old.variant_label

        path = write_run(dir, stamp: "20260101-000015")
        File.open(File.join(path, "run-config.txt"), "a") { |f| f.puts "variant=unknown" }
        unidentified = Importer.new(path).import

        assert_not unidentified.variant_known?
        assert_equal "server unidentified", unidentified.variant_label
      end
    end

    test "only settings explicitly set for the run are kept" do
      Dir.mktmpdir do |dir|
        write_run(dir, stamp: "20260101-000001")
        run = Importer.new(File.join(dir, "20260101-000001")).import

        assert_equal({ "USER_POOL" => "800" }, run.settings)
      end
    end

    test "re-importing replaces derived rows instead of duplicating them" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000002")

        first = Importer.new(path).import
        levels = first.level_stats.count
        samples = first.server_samples.count
        throughput = first.throughput_samples.count

        second = Importer.new(path).import

        assert_equal first.id, second.id
        assert_equal levels, second.level_stats.count
        assert_equal samples, second.server_samples.count
        assert_equal throughput, second.throughput_samples.count
      end
    end

    # Server samples recorded outside the run's own window must not leak in.
    # Before the sampler's process handling was fixed, every run leaked a remote
    # loop that kept appending to the file for as long as an hour, recording
    # load generated by later runs.
    test "server figures ignore samples outside the run window" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000003")

        File.open(File.join(path, "server.csv"), "a") do |file|
          file.puts "#{1_700_100_000},799.0,9.0,9.0,#{99 * 1_048_576},4096"
        end

        run = Importer.new(path).import

        assert_equal 400.0, run.peak_cpu_pct, "peak must come from the run's own window"
        assert_not_equal 99 * 1_048_576, run.peak_wal_bytes
      end
    end

    test "import_all skips directories without metrics" do
      Dir.mktmpdir do |dir|
        write_run(dir, stamp: "20260101-000004")
        FileUtils.mkdir_p(File.join(dir, "not-a-run"))

        runs = Importer.import_all(dir)

        assert_equal 1, runs.length
      end
    end

    test "raises a clear error when metrics are missing" do
      Dir.mktmpdir do |dir|
        assert_raises(RunBundle::MissingMetrics) do
          Importer.new(File.join(dir, "nope")).import
        end
      end
    end
  end
end
