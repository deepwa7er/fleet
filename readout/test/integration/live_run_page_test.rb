require "test_helper"

# One page per run, whether it is happening or happened.
#
# A run in flight and the same run afterwards used to be two pages at two
# addresses. What is worth covering here is that /runs/<stamp> serves both, that
# it picks its source by what exists rather than by which URL was used, and that
# it degrades honestly when the runner is gone.
class LiveRunPageTest < ActionDispatch::IntegrationTest
  IN_FLIGHT = {
    "id" => "20260101-120000",
    "scenario" => "chat",
    "note" => "a hundred people",
    "levers" => { "PEOPLE" => "100" },
    "state" => "running",
    "started_at" => "2026-01-01T12:00:00Z"
  }.freeze

  class FakeRunner
    def initialize(run: nil, progress: nil)
      @run = run
      @progress = progress
    end

    def available? = true
    def health = { "ok" => true, "busy" => @run.present? }
    def log(_id) = "k6 output"

    def run(id)
      raise Harness::Client::Error, "no such run" if @run.nil? || @run["id"] != id

      @run
    end

    def progress(id)
      return nil if @progress.nil? || @run.nil? || @run["id"] != id

      @progress
    end
  end

  def with_runner(runner)
    Harness::Client.define_singleton_method(:new) { |*| runner }
    yield
  ensure
    Harness::Client.singleton_class.remove_method(:new)
  end

  # A run with no row in this database, which the runner is still running.
  test "a run in flight opens at its own address, from the runner" do
    with_runner(FakeRunner.new(run: IN_FLIGHT)) do
      get run_path("20260101-120000")

      assert_response :success
      assert_select "h1", text: "20260101-120000"
      assert_select "p.meta", /a hundred people/

      # Both live halves are frames, because both change while the page is open.
      assert_select "turbo-frame#run-charts[src=?]", charts_run_path("20260101-120000")
      assert_select "turbo-frame#run-status"

      # The charts themselves come from the frame.
      get charts_run_path("20260101-120000")

      assert_select "h2", text: "How long a message takes to arrive"
      assert_select "h2", text: "Requests per second"
      assert_select "[data-coping-chart-url-value=?]",
        progress_run_path("20260101-120000", format: :json), count: 3

      # Polling the progress endpoint, because more data is coming.
      assert_select "[data-coping-chart-live-value=true]", count: 3
    end
  end

  # Everything the page can say, from the first second. The server it is
  # measuring is fixed for the run, so it is rendered once; the request totals
  # climb, so they refresh.
  test "a run in flight shows the server it measures and its running totals" do
    with_runner(FakeRunner.new(run: IN_FLIGHT.merge("server" => {
      "variant" => "tuned",
      "server_image" => "localhost:5000/campfire:6d2cf0a",
      "server_env" => "CAMPFIRE_BATCH_UNREAD=1,RAILS_ENV=performance"
    }))) do
      get run_path("20260101-120000")

      assert_select "h2", text: "Server"
      assert_select ".settings dd", text: "tuned"
      assert_select "turbo-frame#run-requests"
    end
  end

  test "the running request totals are counted live" do
    live = { "requests" => 14_962, "unanswered" => 157 }

    with_runner(FakeRunner.new(run: IN_FLIGHT, progress: live)) do
      get requests_run_path("20260101-120000")

      assert_response :success
      assert_select "h2", text: "Requests"
      assert_select ".settings dd", /14,962/
      assert_select ".settings dd", /157/
      assert_select "[data-poll-active-value=true]", 1
    end
  end

  # Throughput against LOAD is the one chart a live run cannot have: the levels
  # are not known until the run is over.
  test "a run in flight does not claim a load curve" do
    with_runner(FakeRunner.new(run: IN_FLIGHT)) do
      get run_path("20260101-120000")

      assert_select "h2", text: "Requests per second, by load", count: 0
    end
  end

  test "the same address serves the imported run, with no runner involved" do
    run = Run.create!(stamp: "20260101-120000", scenario: "scenarios/chat.js", variant: "tuned")
    run.create_progress!(payload: { "deliveries" => [ { "t" => 1, "v" => 40 } ] })

    # No runner at all: an imported run must open on an instance that never had
    # one, and must not pay for a network call it does not need.
    get run_path("20260101-120000")

    assert_response :success
    assert_select "h1", text: "20260101-120000"
    assert_select "h2", text: "How long a message takes to arrive"
    assert_select "[data-coping-chart-live-value=false]", count: 3
    assert_select "turbo-frame#run-status", count: 0
    assert_select "h2", text: "Requests"
    assert_select "h2", text: "Server"
  end

  test "the charts endpoint serves the runner while live and the row afterwards" do
    live_series = { "deliveries" => [ { "t" => 1, "v" => 9 } ], "duration_s" => 60 }

    with_runner(FakeRunner.new(run: IN_FLIGHT, progress: live_series)) do
      get progress_run_path("20260101-120000", format: :json)

      assert_response :success
      assert_equal [ { "t" => 1, "v" => 9 } ], response.parsed_body["deliveries"]
      assert_equal true, response.parsed_body["running"], "a live run's line must not close to its end"
    end

    run = Run.create!(stamp: "20260101-120000", scenario: "scenarios/chat.js")
    run.create_progress!(payload: { "deliveries" => [ { "t" => 1, "v" => 40 } ], "duration_s" => 60 })

    get progress_run_path("20260101-120000", format: :json)

    assert_equal [ { "t" => 1, "v" => 40 } ], response.parsed_body["deliveries"], "the stored payload wins once it exists"
    assert_equal false, response.parsed_body["running"]
  end

  # The runner keeps its runs in memory, so restarting it forgets them. The page
  # must say so rather than leaving a frame that fails to load.
  test "a forgotten run's status frame says so" do
    Run.create!(stamp: "20260101-120000", scenario: "scenarios/chat.js")

    with_runner(FakeRunner.new(run: nil)) do
      get status_run_path("20260101-120000")

      assert_response :success
      assert_select "p", /no longer has this run/
    end
  end

  test "a stamp that is neither known nor stored is not found" do
    with_runner(FakeRunner.new(run: nil)) do
      get run_path("20260101-999999")

      assert_response :not_found
    end
  end

  test "runs are linked by stamp everywhere" do
    Run.create!(stamp: "20260101-120000", scenario: "scenarios/chat.js")

    get runs_path

    assert_response :success
    assert_select "a[href=?]", "/runs/20260101-120000"
  end
  # k6 ends a run with a non-zero status when its thresholds were crossed. The
  # run completed and every number it produced is good — often it is the most
  # interesting run of the day — so the page must not call it "failed".
  test "a run that crossed thresholds is reported as a result, not a failure" do
    breached = IN_FLIGHT.merge(
      "state" => "failed",
      "threshold_breach" => true,
      "error" => "exited with status 99",
      "finished_at" => "2026-01-01T12:04:00Z",
      "publish_state" => "published"
    )

    with_runner(FakeRunner.new(run: breached)) do
      get status_run_path("20260101-120000")

      assert_response :success
      assert_select "p.meta", /thresholds crossed/
      assert_select "p", /a finding about the server, not a fault in the run/
      assert_select ".figure--bad", count: 0
    end
  end

  test "a run that genuinely broke says so" do
    broken = IN_FLIGHT.merge(
      "state" => "failed",
      "error" => "signal: killed",
      "finished_at" => "2026-01-01T12:04:00Z"
    )

    with_runner(FakeRunner.new(run: broken)) do
      get status_run_path("20260101-120000")

      assert_select ".figure--bad", /did not complete/
    end
  end

  # Results are saved automatically, and publishing starts a few seconds after a
  # run finishes — the final chart payload is computed from k6's whole output
  # first. A frame that stopped polling in that gap froze on the moment before,
  # offering to save results that were already on their way.
  test "results on their way are awaited rather than offered for saving" do
    %w[ pending publishing ].each do |state|
      finishing = IN_FLIGHT.merge(
        "state" => "succeeded",
        "finished_at" => "2026-01-01T12:04:00Z",
        "publish_state" => state
      )

      with_runner(FakeRunner.new(run: finishing)) do
        get status_run_path("20260101-120000")

        assert_select "[data-poll-active-value=true]", { count: 1 }, "must keep watching while #{state}"
        assert_select "p", /Saving the results/
        assert_select "form[action=?]", publish_run_path("20260101-120000"), count: 0
      end
    end
  end

  test "a finished run with its results saved offers the full report" do
    saved = IN_FLIGHT.merge(
      "state" => "succeeded",
      "finished_at" => "2026-01-01T12:04:00Z",
      "publish_state" => "published"
    )

    with_runner(FakeRunner.new(run: saved)) do
      get status_run_path("20260101-120000")

      assert_select "a[href=?]", run_path("20260101-120000"), /Open the full report/
      assert_select "[data-poll-active-value=false]", 1
    end
  end

  # Nothing publishes a stopped run, so asking is the only way to get its
  # partial results in — and the button must still be there for that.
  test "a run nothing will publish still offers to save" do
    stopped = IN_FLIGHT.merge(
      "state" => "canceled",
      "finished_at" => "2026-01-01T12:04:00Z",
      "publish_state" => ""
    )

    with_runner(FakeRunner.new(run: stopped)) do
      get status_run_path("20260101-120000")

      assert_select "form[action=?]", publish_run_path("20260101-120000")
      assert_select "[data-poll-active-value=false]", 1
    end
  end

  # A run builds its company before it measures it, when the database holds a
  # different size. That takes seconds, and a page that showed "running" through
  # it would be reporting a load test that had not started.
  SEEDING = {
    "id" => "20260101-120000", "scenario" => "enterprise", "state" => "seeding",
    "employees" => 300, "step" => "creating 300 employees",
    "levers" => { "VUS" => "300" }, "started_at" => "2026-01-01T12:00:00Z"
  }.freeze

  test "a run building its company says so, and what it is doing" do
    with_runner(FakeRunner.new(run: SEEDING)) do
      get status_run_path("20260101-120000")

      assert_response :success
      assert_select "p.meta", /building a company of 300 employees/
      assert_select "p.meta", /creating 300 employees/
      assert_select "[data-poll-active-value=true]", 1
    end
  end

  # Stopping is the only thing to offer: there are no results to save yet.
  test "a run building its company can be stopped and nothing else" do
    with_runner(FakeRunner.new(run: SEEDING)) do
      get status_run_path("20260101-120000")

      assert_select "form[action=?]", cancel_run_path("20260101-120000")
      assert_select "form[action=?]", publish_run_path("20260101-120000"), count: 0
    end
  end

  test "the charts wait until there is a load test to chart" do
    with_runner(FakeRunner.new(run: SEEDING)) do
      get charts_run_path("20260101-120000")

      assert_response :success
      assert_select "canvas", false
      assert_select "p", /charts appear once the load test starts/

      # Still watching, because this is the state that changes on its own.
      assert_select "[data-poll-active-value=true]", 1
    end
  end

  # The bug this frame exists for: the page decided once whether there was
  # anything to draw, so a run that started measuring while you were watching
  # kept showing the note until you refreshed.
  test "the charts replace the note by themselves when the run starts" do
    with_runner(FakeRunner.new(run: SEEDING)) do
      get charts_run_path("20260101-120000")
      assert_select "canvas", false
    end

    running = SEEDING.merge("state" => "running", "step" => nil)

    with_runner(FakeRunner.new(run: running)) do
      get charts_run_path("20260101-120000")

      assert_select "canvas", 3
      # And stops watching: the canvases must not be rebuilt every two seconds.
      assert_select "[data-poll-active-value=false]", 1
    end
  end
end
