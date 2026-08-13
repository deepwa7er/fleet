require "test_helper"

class RunNarrativeTest < ActiveSupport::TestCase
  def build_run(peak_cpu_pct: nil, levels: [])
    run = Run.create!(
      stamp: "n-#{SecureRandom.hex(4)}",
      scenario: "scenarios/ramp.js",
      peak_cpu_pct: peak_cpu_pct
    )
    levels.each { |attrs| run.level_stats.create!(**attrs) }
    run
  end

  test "a healthy run says so and asks for more load" do
    run = build_run(peak_cpu_pct: 120.0, levels: [
      { vus: 25, room_open_p95: 140, post_message_p95: 420, cpu_avg_pct: 20 },
      { vus: 200, room_open_p95: 130, post_message_p95: 440, cpu_avg_pct: 90 }
    ])
    narrative = RunNarrative.new(run)

    assert_match(/Handled everything/, narrative.headline)
    assert_match(/200 people/, narrative.headline)
    assert(narrative.suggestions.any? { |s| s.match?(/Push harder/) })
  end

  test "a run with a cliff names where it broke" do
    run = build_run(peak_cpu_pct: 558.0, levels: [
      { vus: 200, room_open_p95: 255, post_message_p95: 743, cpu_avg_pct: 164 },
      { vus: 400, room_open_p95: 4301, post_message_p95: 14336, cpu_avg_pct: 513 }
    ])
    narrative = RunNarrative.new(run)

    assert_match(/Comfortable up to 200 people/, narrative.headline)
    assert_match(/Falls over at 400 people/, narrative.headline)
    assert(narrative.findings.any? { |f| f[:title].match?(/Why it falls over/) })
  end

  # The bug this exists to prevent: a single level has nothing to compare
  # against, so a purely relative check called nine-second page loads healthy.
  test "a single slow level is not reported as healthy" do
    run = build_run(peak_cpu_pct: 543.0, levels: [
      { vus: 1000, room_open_p95: 9035, post_message_p95: 14495, cpu_avg_pct: 509 }
    ])
    narrative = RunNarrative.new(run)

    assert_no_match(/Handled everything/, narrative.headline)
    assert_match(/Struggling/, narrative.headline)
    assert_match(/9.0 seconds/, narrative.headline)
  end

  test "no claim that reading is cheap when nothing behaved" do
    run = build_run(peak_cpu_pct: 543.0, levels: [
      { vus: 1000, room_open_p95: 9035, post_message_p95: 14495, cpu_avg_pct: 509 }
    ])

    titles = RunNarrative.new(run).findings.map { |f| f[:title] }
    assert_not_includes titles, "Reading pages is cheap"
  end

  # Judging against the machine's 8 cores instead of Puma's 6 workers is what
  # made the dashboard call a saturated app "not compute bound".
  test "saturation is judged against what the app can use, not the machine" do
    run = build_run(peak_cpu_pct: 558.0, levels: [
      { vus: 200, room_open_p95: 255, cpu_avg_pct: 164 },
      { vus: 400, room_open_p95: 4301, cpu_avg_pct: 513 }
    ])

    assert run.cpu_saturated?, "558% of a 600% ceiling should count as saturated"
    body = RunNarrative.new(run).findings.find { |f| f[:title].match?(/How hard/) }[:body]
    assert_match(/working flat out/, body)
    assert(run.peak_cpu_share_of_capacity.round >= 90)
  end

  test "an idle run is not described as working flat out" do
    run = build_run(peak_cpu_pct: 60.0, levels: [
      { vus: 10, room_open_p95: 140, post_message_p95: 400, cpu_avg_pct: 10 }
    ])

    assert_not run.cpu_saturated?
    body = RunNarrative.new(run).findings.find { |f| f[:title].match?(/How hard/) }[:body]
    assert_match(/room to work harder/, body)
  end

  test "milliseconds are rendered as seconds" do
    run = build_run(levels: [ { vus: 10, room_open_p95: 4301, post_message_p95: 400 } ])

    assert_match(/4.3 seconds/, RunNarrative.new(run).headline)
  end

  test "an empty run says nothing rather than inventing a verdict" do
    narrative = RunNarrative.new(build_run)

    assert_not narrative.present?
    assert_empty narrative.findings
    assert_match(/did not get far enough/, narrative.headline)
  end
end
