require "test_helper"

module Analysis
  class LevelBreakdownTest < ActiveSupport::TestCase
    # A stand-in for MetricsFile so the segmentation rules can be tested against
    # hand-built timelines rather than an 80MB fixture.
    class FakeMetrics
      attr_reader :vu_timeline, :latency_samples, :counter_samples

      def initialize(vu_timeline, latency_samples = {}, counter_samples = {})
        @vu_timeline = vu_timeline
        @latency_samples = Hash.new { |h, k| h[k] = [] }.merge(latency_samples)
        @counter_samples = Hash.new { |h, k| h[k] = [] }.merge(counter_samples)
      end
    end

    def sample(at, value)
      MetricsFile::Sample.new(at, value)
    end

    test "splits the timeline into contiguous segments" do
      timeline = [ [ 0, 10 ], [ 1, 10 ], [ 2, 20 ], [ 3, 20 ], [ 4, 10 ] ]
      segments = LevelBreakdown.new(FakeMetrics.new(timeline)).segments

      assert_equal [ 10, 20, 10 ], segments.map(&:vus)
      assert_equal [ 0, 2, 4 ], segments.map(&:started_at)
    end

    # The bug this guards: a ramp passes through the same level twice, once up
    # and once down. Merging those into one window counts the entire middle of
    # the run as elapsed time and deflates throughput to nonsense.
    test "a level visited twice yields two separate segments" do
      timeline = []
      (0..100).each { |t| timeline << [ t, 50 ] }        # hold at 50
      (101..200).each { |t| timeline << [ t, 100 ] }     # hold at 100
      (201..300).each { |t| timeline << [ t, 50 ] }      # back down through 50

      breakdown = LevelBreakdown.new(FakeMetrics.new(timeline))
      fifties = breakdown.segments.select { |s| s.vus == 50 }

      assert_equal 2, fifties.length
      assert_equal 100, fifties.first.duration
      assert_equal 99, fifties.last.duration
    end

    test "ignores levels held for less than the minimum" do
      timeline = []
      (0..100).each { |t| timeline << [ t, 25 ] }
      (101..110).each { |t| timeline << [ t, 60 ] }  # only 9s: in transit
      (111..200).each { |t| timeline << [ t, 100 ] }

      breakdown = LevelBreakdown.new(FakeMetrics.new(timeline))

      assert_equal [ 25, 100 ], breakdown.plateau_levels
    end

    test "trims the settling period from the front of each window" do
      timeline = (0..100).map { |t| [ t, 25 ] }

      # One slow sample early (settling) and fast samples later. With the
      # settling quarter trimmed, the slow one must not reach the percentile.
      latencies = {
        "campfire_room_open" => [ sample(5, 5000.0) ] + (30..100).map { |t| sample(t, 100.0) }
      }

      # Requests alongside, as k6 always records them: a latency sample is a
      # measurement OF a request, so a window holding one and not the other does
      # not occur. Without them the level would be dropped as idle.
      counters = { "http_reqs" => (0..100).map { |t| sample(t, 1.0) } }

      breakdown = LevelBreakdown.new(FakeMetrics.new(timeline, latencies, counters))
      level = breakdown.levels.first

      assert_equal 25, level.vus
      assert_equal 100.0, level.latencies[:room_open_p95]
    end

    test "computes throughput over the trimmed window" do
      timeline = (0..100).map { |t| [ t, 25 ] }
      # 76 requests spread across the untrimmed portion (t=25..100).
      counters = { "http_reqs" => (25..100).map { |t| sample(t, 1.0) } }

      breakdown = LevelBreakdown.new(FakeMetrics.new(timeline, {}, counters))
      level = breakdown.levels.first

      assert_equal 75, level.window_seconds
      assert_in_delta 1.0, level.requests_per_second, 0.05
    end

    # The bug this guards: gracefulRampDown and gracefulStop leave a few VUs
    # finishing their last session for a minute after the run is over. That is a
    # constant VU count held well past the minimum, so it looked exactly like a
    # level -- and being a level at zero throughput, it drew a load curve rising
    # out of the run's own dying tail.
    test "a plateau with no traffic in it is not a level" do
      timeline = []
      (0..100).each { |t| timeline << [ t, 500 ] }      # the run
      (101..180).each { |t| timeline << [ t, 2 ] }      # stragglers leaving

      # Requests stop when the run does, which is what makes the tail idle.
      counters = { "http_reqs" => (0..100).map { |t| sample(t, 1.0) } }
      breakdown = LevelBreakdown.new(FakeMetrics.new(timeline, {}, counters))

      assert_equal [ 2, 500 ], breakdown.plateau_levels, "both are plateaus by duration"
      assert_equal [ 500 ], breakdown.levels.map(&:vus), "only one of them saw traffic"
    end

    test "reports no levels when nothing was held long enough" do
      timeline = (0..10).map { |t| [ t, 25 ] }

      assert_empty LevelBreakdown.new(FakeMetrics.new(timeline)).levels
    end
  end
end
