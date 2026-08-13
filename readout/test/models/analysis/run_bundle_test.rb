require "test_helper"
require "tmpdir"

module Analysis
  # A bundle is the only thing that crosses between a generator machine and this
  # dashboard, so its shape is a contract rather than an implementation detail.
  # These tests cover the round trip and the ways a bad one must be refused.
  class RunBundleTest < ActiveSupport::TestCase
    include ResultsDirectory

    test "carries a run's parsed numbers without a database" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000000", machine: "desktop")
        bundle = RunBundle.build(path)

        assert_equal "20260101-000000", bundle.stamp
        assert_equal "scenarios/ramp.js", bundle.run["scenario"]
        assert_equal "desktop", bundle.machine

        assert_equal [ 10, 20 ], bundle.levels.map { |level| level["vus"] }
        assert_equal 200.0, bundle.levels.last["room_open_p95"]

        # One entry per second of the run, so a stall reads as zero rather than
        # as a gap the chart interpolates across.
        assert_equal 160, bundle.throughput_samples.length
        assert_equal 160, bundle.server_samples.length
      end
    end

    # The point of the split: a generator parses its own results with no Rails
    # app, no gems and no database, and sends only this.
    test "survives a JSON round trip unchanged" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000001", machine: "mac")
        sent = RunBundle.build(path)
        received = RunBundle.from_json(JSON.generate(sent.to_h))

        assert_equal sent.to_h, received.to_h
        assert_equal "mac", received.machine
      end
    end

    # Timestamps cross a machine boundary here, and two boxes need not agree on
    # a time zone. Epoch seconds are the one representation that cannot be read
    # as a different instant at the other end.
    test "puts times on the wire as epoch seconds" do
      Dir.mktmpdir do |dir|
        path = write_run(dir, stamp: "20260101-000002", base: 1_700_000_000)
        bundle = RunBundle.build(path)

        assert_equal 1_700_000_000, bundle.run["started_at"]
        assert_equal 1_700_000_000, bundle.throughput_samples.first["at"]
      end
    end

    # A wrong number on this dashboard is worse than a missing one, because
    # nothing about it looks wrong. A bundle written to a shape this code does
    # not know is refused rather than read as far as it parses.
    test "refuses a format it does not know" do
      error = assert_raises(RunBundle::Invalid) do
        RunBundle.from_h({ "format" => 99, "stamp" => "20260101-000000", "run" => {} })
      end

      assert_match(/99/, error.message)
    end

    # The stamp is the run's identity in four places at once — the results
    # directory, the runner's id, the URL, and the unique key in this database —
    # and it arrives over the network.
    test "refuses a stamp that is not one" do
      assert_raises(RunBundle::Invalid) do
        RunBundle.from_h({
          "format" => RunBundle::FORMAT,
          "stamp" => "../../etc/passwd",
          "run" => {}
        })
      end
    end

    test "refuses a bundle with no run" do
      assert_raises(RunBundle::Invalid) do
        RunBundle.from_h({ "format" => RunBundle::FORMAT, "stamp" => "20260101-000000" })
      end
    end

    # Charts are the part that may legitimately be absent: any run that finished
    # before the runner wrote progress.json has none, and those pages say so.
    test "accepts a run with no chart payload" do
      bundle = RunBundle.from_h({
        "format" => RunBundle::FORMAT,
        "stamp" => "20260101-000000",
        "run" => { "scenario" => "scenarios/chat.js" }
      })

      assert_nil bundle.progress
      assert_empty bundle.levels
    end

    test "refuses a directory that holds no metrics" do
      Dir.mktmpdir do |dir|
        assert_raises(RunBundle::MissingMetrics) { RunBundle.build(File.join(dir, "nope")) }
      end
    end
  end
end
