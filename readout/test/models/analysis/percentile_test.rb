require "test_helper"

module Analysis
  class PercentileTest < ActiveSupport::TestCase
    test "returns nil for no values" do
      assert_nil Percentile.of([], 95)
    end

    test "returns the only value for a single sample" do
      assert_equal 42.0, Percentile.of([ 42.0 ], 95)
    end

    test "interpolates between ranks" do
      # For 1..10, rank = 9 * 0.95 = 8.55, so the result sits between the 9th
      # and 10th values: 9 + (10 - 9) * 0.55.
      assert_in_delta 9.55, Percentile.of((1..10).map(&:to_f), 95), 0.001
    end

    test "handles unsorted input" do
      assert_equal Percentile.of([ 3.0, 1.0, 2.0 ], 50), Percentile.of([ 1.0, 2.0, 3.0 ], 50)
    end

    test "the median of an odd-length series is the middle value" do
      assert_equal 3.0, Percentile.of([ 1.0, 2.0, 3.0, 4.0, 5.0 ], 50)
    end
  end
end
