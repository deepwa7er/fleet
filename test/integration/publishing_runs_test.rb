require "test_helper"
require "tmpdir"

# How results reach this dashboard from a machine that is not this one.
#
# Worth covering end to end because the alternative it replaced — shipping the
# whole SQLite database up by rsync — failed silently rather than loudly: with
# two generators, whichever published last replaced the other's history and
# nothing about the result looked wrong. The properties below are what make more
# than one generator safe.
class PublishingRunsTest < ActionDispatch::IntegrationTest
  include ResultsDirectory

  TOKEN = "a-shared-secret".freeze

  setup do
    @token_file = Tempfile.new("ingest-token")
    @token_file.write(TOKEN)
    @token_file.flush

    @previous_token_file = Rails.configuration.x.ingest.token_file
    Rails.configuration.x.ingest.token_file = @token_file.path
    IngestToken.reload!
  end

  teardown do
    Rails.configuration.x.ingest.token_file = @previous_token_file
    IngestToken.reload!
    @token_file.close!
  end

  test "stores a published run and says what it did with it" do
    post api_runs_path, params: bundle_for("20260101-000000", machine: "desktop"),
                        headers: published_by(TOKEN)

    assert_response :created
    body = JSON.parse(response.body)
    assert_equal "20260101-000000", body["stamp"]
    assert_equal "desktop", body["machine"]
    assert_equal 2, body["levels"]

    run = Run.find_by(stamp: "20260101-000000")
    assert_equal "desktop", run.machine
    assert_equal [ 10, 20 ], run.level_stats.pluck(:vus)
    assert_equal 160, run.throughput_samples.count
  end

  # The property the whole design turns on. Two machines publish into one
  # dashboard, and neither may remove the other's runs.
  test "runs from two machines accumulate rather than replace each other" do
    post api_runs_path, params: bundle_for("20260101-000000", machine: "mac"),
                        headers: published_by(TOKEN)
    post api_runs_path, params: bundle_for("20260102-000000", machine: "desktop"),
                        headers: published_by(TOKEN)

    assert_equal %w[ mac desktop ], Run.order(:stamp).pluck(:machine)
  end

  # Publishing is retried by hand after a failure, and runs publish themselves on
  # completion — so the same bundle arriving twice has to be ordinary rather than
  # something that duplicates a run's levels or its per-second series.
  test "publishing the same run twice replaces it in place" do
    2.times do
      post api_runs_path, params: bundle_for("20260101-000000", machine: "mac"),
                          headers: published_by(TOKEN)
      assert_response :created
    end

    assert_equal 1, Run.count
    assert_equal 2, Run.sole.level_stats.count
    assert_equal 160, Run.sole.throughput_samples.count
  end

  test "refuses a bundle with no token" do
    post api_runs_path, params: bundle_for("20260101-000000"),
                        headers: { "CONTENT_TYPE" => "application/json" }

    assert_response :unauthorized
    assert_equal 0, Run.count
  end

  test "refuses a bundle with the wrong token" do
    post api_runs_path, params: bundle_for("20260101-000000"),
                        headers: published_by("not-the-secret")

    assert_response :unauthorized
    assert_equal 0, Run.count
  end

  # An instance nobody publishes to must take nothing at all, rather than taking
  # everything because it has no secret to compare against.
  test "refuses everything when no token is configured" do
    Rails.configuration.x.ingest.token_file = nil
    IngestToken.reload!

    post api_runs_path, params: bundle_for("20260101-000000"),
                        headers: published_by(TOKEN)

    assert_response :service_unavailable
    assert_equal 0, Run.count
  end

  # A publish that fails must name what was wrong: the results still exist on the
  # generator, and the fix is to send them again.
  test "names what was wrong with a bundle it cannot read" do
    post api_runs_path, params: '{"format": 99, "stamp": "20260101-000000", "run": {}}',
                        headers: published_by(TOKEN)

    assert_response :unprocessable_content
    assert_match(/99/, JSON.parse(response.body)["error"])
    assert_equal 0, Run.count
  end

  test "refuses a body that is not JSON at all" do
    post api_runs_path, params: "not json", headers: published_by(TOKEN)

    assert_response :unprocessable_content
    assert_equal 0, Run.count
  end

  private

  def published_by(token)
    { "X-Readout-Token" => token, "CONTENT_TYPE" => "application/json" }
  end

  def bundle_for(stamp, machine: "mac")
    Dir.mktmpdir do |dir|
      JSON.generate(Analysis::RunBundle.build(write_run(dir, stamp: stamp, machine: machine)).to_h)
    end
  end
end
