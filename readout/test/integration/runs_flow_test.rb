require "test_helper"

class RunsFlowTest < ActionDispatch::IntegrationTest
  setup do
    @run = Run.create!(
      stamp: "20260101-120000",
      scenario: "scenarios/ramp.js",
      target: "http://example.test",
      peak_cpu_pct: 558.0,
      total_requests: 14_962,
      config: '{"USER_POOL":"800"}'
    )
    @run.level_stats.create!(vus: 200, room_open_p95: 255, requests_per_second: 34.0, error_count: 0)
    @run.level_stats.create!(vus: 400, room_open_p95: 4301, requests_per_second: 43.3, error_count: 1)
    @run.level_stats.create!(vus: 800, room_open_p95: 17_710, requests_per_second: 40.0, error_count: 21)
  end

  test "index lists runs" do
    get root_path

    assert_response :success
    assert_select ".run-title", text: "20260101-120000"
  end

  # The results page is deliberately minimal: name, settings, the two charts a
  # run in flight shows, and the request figures only a finished run can offer.
  # Everything else was removed on purpose.
  test "show is the name, the settings, and the throughput chart" do
    get run_path(@run)

    assert_response :success
    assert_select "h1", text: @run.stamp
    assert_select "h2", text: "Settings"
    assert_select "h2", text: "Requests per second, by load"
    assert_select "svg.chart--plotted"
  end

  test "show states the settings in plain words rather than variable names" do
    get run_path(@run)

    assert_select ".settings dt", text: "distinct accounts used"
    assert_select ".settings dd", text: "800"
  end

  # Units are explained on the page rather than assumed.
  test "show explains what requests per second means" do
    get run_path(@run)

    assert_select "p", /how many simulated users were browsing/
  end

  test "show carries no analysis tables or verdicts any more" do
    get run_path(@run)

    assert_select "table", false
    assert_select ".finding-title", false
    assert_select ".key-row", false
  end

  # A chat or enterprise run holds one population from start to finish, so there
  # is no curve to draw against load. Charted that way it was a lone dot, and a
  # run cancelled before its level was held long enough had nothing at all --
  # which is what "the chart is empty" meant.
  test "a single-level run is charted against time instead of against load" do
    single = Run.create!(stamp: "20260101-130000", scenario: "scenarios/enterprise.js")
    single.level_stats.create!(vus: 250, requests_per_second: 42.0, error_count: 0)
    60.times { |second| single.throughput_samples.create!(at: Time.zone.at(1_700_000_000 + second), requests: 40 + second) }

    get run_path(single)

    assert_response :success
    assert_select "svg.chart--plotted polyline"
    assert_select "p", /drawn against time rather than against people/
    assert_select "p", /measured at/
  end

  # The one case that genuinely has nothing to show says so, rather than
  # rendering an empty frame.
  test "a run that recorded nothing says so" do
    nothing = Run.create!(stamp: "20260101-140000", scenario: "scenarios/enterprise.js")

    get run_path(nothing)

    assert_response :success
    assert_select "svg", false
    assert_select "p", /did not last long enough/
  end

  # The bug this pair exists to keep fixed: a run looked like two different runs
  # depending on whether you arrived from the run list or from having just
  # launched it. The launched view is the one that is right, so the historical
  # page draws the same two charts, from the payload the runner computed.
  test "a finished run carries the same charts as a run in flight" do
    @run.create_progress!(payload: { "deliveries" => [ { "t" => 1, "v" => 40 } ], "duration_s" => 60 })

    get run_path(@run)

    assert_response :success
    assert_select "h2", text: "How long a message takes to arrive"
    assert_select "h2", text: "Messages delivered per second"
    assert_select "[data-coping-chart-url-value=?]", progress_run_path(@run, format: :json), count: 3

    # Nothing more is coming, so the chart is told not to poll for it.
    assert_select "[data-coping-chart-live-value=false]", count: 3
  end

  test "the stored series are served in the shape the chart reads" do
    @run.create_progress!(payload: { "deliveries" => [ { "t" => 1, "v" => 40 } ], "duration_s" => 60 })

    get progress_run_path(@run, format: :json)

    assert_response :success
    payload = response.parsed_body
    assert_equal [ { "t" => 1, "v" => 40 } ], payload["deliveries"]
    assert_equal 60, payload["duration_s"]
    assert_equal false, payload["running"], "a finished run's line must be drawn to its end"
  end

  # Runs imported before the runner saved its payload have no series at all, and
  # cannot get them from here — the raw output is on the machine that generated
  # the load. Saying so beats an empty frame, which reads as a run in which
  # nothing happened.
  test "a run with no stored series says so instead of drawing an empty chart" do
    get run_path(@run)

    assert_response :success
    assert_select "canvas", false
    assert_select "p", /No delivery charts for this run/
    assert_select "p", /rebuild-progress/
  end

  test "asking for series a run does not have is a not-found rather than an empty chart" do
    get progress_run_path(@run, format: :json)

    assert_response :not_found
  end

  test "a run names the server it measured, on its page and in the list" do
    @run.update!(
      variant: "tuned",
      server_image: "localhost:5000/campfire:6d2cf0a",
      server_env: "CAMPFIRE_BATCH_UNREAD=1,RAILS_ENV=performance"
    )

    get run_path(@run)
    assert_select "h2", text: "Server"
    assert_select ".settings dd", text: "tuned"
    assert_select ".settings dd", text: "localhost:5000/campfire:6d2cf0a"

    get runs_path
    assert_select ".run .meta", /tuned/
  end

  # Saying so beats implying a comparison the run cannot support.
  test "a run with no recorded server says so rather than looking comparable" do
    get run_path(@run)

    assert_response :success
    assert_select ".settings dd", text: "server not recorded"
    assert_select "p", /cannot be compared against one that does/
  end

  test "import reports a missing results directory instead of failing silently" do
    original = Rails.configuration.x.campfire_stress.results_root
    Rails.configuration.x.campfire_stress.results_root = "/nonexistent/path"

    post import_runs_path

    assert_redirected_to runs_path
    assert_match(/not found/, flash[:alert])
  ensure
    Rails.configuration.x.campfire_stress.results_root = original
  end
end
