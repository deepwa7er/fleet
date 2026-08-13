require "test_helper"

class RunTest < ActiveSupport::TestCase
  def build_run(peak_cpu_pct: nil, levels: [])
    run = Run.create!(stamp: "test-#{SecureRandom.hex(4)}", peak_cpu_pct: peak_cpu_pct)
    levels.each { |attrs| run.level_stats.create!(**attrs) }
    run
  end

  test "knee is nil when nothing degraded" do
    run = build_run(levels: [
      { vus: 25, room_open_p95: 100 },
      { vus: 50, room_open_p95: 120 },
      { vus: 100, room_open_p95: 110 }
    ])

    assert_nil run.knee
  end

  test "knee is the first level past the degradation factor" do
    run = build_run(levels: [
      { vus: 100, room_open_p95: 200 },
      { vus: 200, room_open_p95: 250 },
      { vus: 400, room_open_p95: 4300 },
      { vus: 800, room_open_p95: 17_000 }
    ])

    assert_equal 400, run.knee.vus
  end

  test "knee needs at least two levels to compare" do
    run = build_run(levels: [ { vus: 25, room_open_p95: 5000 } ])

    assert_nil run.knee
  end

  # The judgement that matters, and the one this originally had backwards.
  #
  # Puma runs 6 workers and Ruby's global lock caps each at one core, so the app
  # cannot exceed ~600% however hard it works. 558% is therefore 93% of
  # everything it is allowed to use — saturated — even though it is only 70% of
  # the 8-core machine. Judging against the machine called a maxed-out app "not
  # compute bound" and sent the diagnosis off to the network instead.
  test "cpu near the app's own ceiling counts as saturation" do
    run = build_run(peak_cpu_pct: 558.0)

    assert run.cpu_saturated?,
      "558% of the #{Analysis.cpu_ceiling_pct}% this app can use should be saturated"
    assert_operator run.peak_cpu_share_of_capacity.round, :>=, 90
    assert_operator run.peak_cpu_share_of_host.round, :<=, 75
  end

  test "cpu well below the app's ceiling is not saturation" do
    run = build_run(peak_cpu_pct: 240.0)

    assert_not run.cpu_saturated?
  end

  test "degradation with genuine cpu headroom is reported as not compute bound" do
    run = build_run(peak_cpu_pct: 240.0, levels: [
      { vus: 200, room_open_p95: 255 },
      { vus: 400, room_open_p95: 4300 }
    ])

    assert run.degraded_without_cpu_saturation?
  end

  # A single level has nothing to compare against, so the relative knee test
  # cannot fire. An absolute floor is what stops nine-second pages reading as
  # healthy.
  test "a single slow level is a breaking point even without a knee" do
    run = build_run(levels: [ { vus: 1000, room_open_p95: 9035 } ])

    assert_nil run.knee
    assert_equal 1000, run.breaking_point.vus
    assert_not run.healthy?
  end

  test "a single fast level is healthy" do
    run = build_run(levels: [ { vus: 50, room_open_p95: 140 } ])

    assert_nil run.breaking_point
    assert run.healthy?
  end

  test "degradation with saturated cpu is not flagged as another cause" do
    run = build_run(peak_cpu_pct: 790.0, levels: [
      { vus: 200, room_open_p95: 255 },
      { vus: 400, room_open_p95: 4300 }
    ])

    assert_not run.degraded_without_cpu_saturation?
  end

  test "throughput that keeps climbing has not plateaued" do
    run = build_run(levels: [
      { vus: 100, megabytes_per_second: 2.8 },
      { vus: 200, megabytes_per_second: 6.7 },
      { vus: 400, megabytes_per_second: 13.0 }
    ])

    assert_not run.throughput_plateaued?
  end

  test "throughput flat between the top levels has plateaued" do
    run = build_run(levels: [
      { vus: 200, megabytes_per_second: 7.4 },
      { vus: 400, megabytes_per_second: 19.6 },
      { vus: 800, megabytes_per_second: 20.2 }
    ])

    assert run.throughput_plateaued?
  end

  test "settings parses stored json and survives garbage" do
    run = Run.create!(stamp: "cfg-1", config: '{"VUS":"800"}')
    assert_equal({ "VUS" => "800" }, run.settings)

    broken = Run.create!(stamp: "cfg-2", config: "not json")
    assert_empty broken.settings
  end
end
