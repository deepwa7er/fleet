require "test_helper"

# The review's deploy + landing readouts (card #122): the one-line summary
# derived from the bridge's deploy record — "deploy in progress" while jobs
# run, the terminal "deploy complete" with a failure count — and the GitHub
# commit link that exists only once the push is in. The predicates are
# pinned here; the rendering is pinned in the integration test.
class ChangeHelperTest < ActionView::TestCase
  def change(deploy: nil, landed: nil)
    { repo: "fleet", card: 122, state: "shipped", deploy: deploy, landed: landed }
  end

  test "deploy readout is nil when no deploy was recorded" do
    assert_nil deploy_readout(change)
  end

  test "deploy readout is nil when the trigger failed — that has its own line" do
    assert_nil deploy_readout(change(deploy: { error: "tugboat daemon unreachable", services: [] }))
  end

  test "deploy readout is nil when nothing was started" do
    assert_nil deploy_readout(change(deploy: { error: nil, services: [] }))
  end

  test "deploy readout reports in progress while any job is in flight" do
    readout = deploy_readout(change(deploy: {
      services: [
        { name: "breakwater", jobId: "bw-1", outcome: nil },
        { name: "tugboat", jobId: "tb-2", outcome: { ok: true } }
      ]
    }))
    assert_equal "deploy in progress · 2 services", readout[:text]
    refute readout[:failed]
  end

  test "deploy readout is terminal once every outcome is in" do
    readout = deploy_readout(change(deploy: {
      services: [
        { name: "breakwater", jobId: "bw-1", outcome: { ok: true } },
        { name: "tugboat", jobId: "tb-2", outcome: { ok: true } }
      ]
    }))
    assert_equal "deploy complete", readout[:text]
    refute readout[:failed]
  end

  test "deploy readout counts failed services in the terminal line" do
    readout = deploy_readout(change(deploy: {
      services: [
        { name: "breakwater", jobId: "bw-1", outcome: { ok: true } },
        { name: "tugboat", jobId: "tb-2", outcome: { ok: false, message: "build failed" } }
      ]
    }))
    assert_equal "deploy complete · 1 service failed", readout[:text]
    assert readout[:failed]
  end

  test "a service already deploying when approved is not counted as failed" do
    readout = deploy_readout(change(deploy: {
      services: [
        { name: "breakwater", jobId: "bw-1", outcome: { ok: true } },
        { name: "sonar", jobId: nil, status: "in_progress", outcome: nil }
      ]
    }))
    assert_equal "deploy complete", readout[:text]
    refute readout[:failed]
  end

  test "commit url exists once the tip is in, and never before or for an unresolved tip" do
    assert_equal "https://github.com/deepwa7er/fleet/commit/8f3b68b246b340c3812c5a11ceda90db27673be0",
      landed_commit_url(change(landed: { tip: "8f3b68b246b340c3812c5a11ceda90db27673be0" }))
    assert_nil landed_commit_url(change)
    assert_nil landed_commit_url(change(landed: { tip: "(unresolved)" }))
    assert_nil landed_commit_url(change(landed: { tip: "" }))
  end
end
