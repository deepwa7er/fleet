require "test_helper"

# The review (DW-002 §5–6): the annotated diff, the header's claims carried
# as claims, and the verbs. Stubbing follows the house convention; the
# assertions pin the design contract — annotation placement, the claim
# labels, which verbs exist in which states, and the flash copy that tells a
# bridge refusal from a dead socket.
class ChangesTest < ActionDispatch::IntegrationTest
  DIFF = <<~DIFF.freeze
    diff --git a/app/models/harness.rb b/app/models/harness.rb
    @@ -1,2 +1,3 @@
     class Harness
    +  def available_models = cache.fetch(:models)
     end
  DIFF

  def change_fixture(state: "in_review", session: nil, annotations: [], with_stranded: false)
    all = annotations
    all += [ { id: "a2", path: "gone.rb", line: 9, side: "new", text: "stranded note" } ] if with_stranded
    {
      repo: "fleet",
      card: 81,
      title: "pi model picker",
      session: session,
      state: state,
      path: "/home/deepwater/code/fleet",
      updatedAt: "2026-08-23T12:00:00Z",
      lastLanding: nil,
      cardComment: nil,
      rounds: [
        {
          n: 1, author: "agent", note: nil,
          gatesRan: [ "cargo test", "clippy" ],
          worthKnowing: [ "+1 dependency (serde_yaml)" ],
          annotations: all,
          commit: { commitId: "abc123", description: "round 1" }
        }
      ]
    }
  end

  ANNOTATION = { id: "a1", path: "app/models/harness.rb", line: 2, side: "new", text: "cached because the phone re-polls" }.freeze

  def stub_show(change, diff: DIFF, &block)
    BridgeClient.stub(:change, change) do
      BridgeClient.stub(:change_diff, { diff: diff }, &block)
    end
  end

  test "show renders the header claims, the diff, and the annotation at its line" do
    stub_show(change_fixture(annotations: [ ANNOTATION ])) do
      get change_path("fleet", 81)
    end

    assert_response :success
    assert_select "h2", text: "pi model picker"
    assert_select ".instrumentation", text: /fleet #81 · round 1 · in review/
    # The claim is labelled as a claim — "agent ran", never a verdict.
    assert_select ".instrumentation", text: /agent ran cargo test · clippy/
    assert_select ".worth-knowing li", text: "+1 dependency (serde_yaml)"
    assert_select ".diff-file-header", text: "app/models/harness.rb"
    assert_select ".diff-line--add .diff-text", text: /available_models/
    assert_select ".annotation", text: /cached because the phone re-polls/
    # The verbs exist: in review is the state that blocks on a human.
    assert_select "form[action='/changes/fleet/81/approve']"
    assert_select "form[action='/changes/fleet/81/request_changes'] textarea[name='note']"
    assert_select ".edit-path", text: "/home/deepwater/code/fleet"
  end

  test "a stranded annotation is shown and labelled, never dropped" do
    stub_show(change_fixture(with_stranded: true)) do
      get change_path("fleet", 81)
    end

    assert_select ".instrumentation--danger", text: /annotations whose lines this diff no longer shows/
    assert_select ".annotation", text: /gone\.rb:9.*stranded note/m
  end

  test "the cumulative view renders the diff without inline annotations" do
    stub_show(change_fixture(annotations: [ ANNOTATION ])) do
      get change_path("fleet", 81, round: "all")
    end

    assert_response :success
    assert_select ".round-nav-current", text: "cumulative"
    assert_select ".diff-line--add", 1
    # Annotations are positioned in a round's diff; they have no coordinates
    # in the cumulative one, so none render.
    assert_select ".annotation", 0
  end

  test "no verbs outside review, and the landing failure reads out its reason" do
    change = change_fixture(state: "working").merge(
      lastLanding: { ok: false, reason: "the rebase onto main conflicts; resolve it as the next round", conflicts: [] }
    )
    stub_show(change) do
      get change_path("fleet", 81)
    end

    assert_select "form[action='/changes/fleet/81/approve']", 0
    assert_select ".instrumentation--danger", text: /landing failed — the rebase onto main conflicts/
  end

  test "the review embeds the bound session's transcript" do
    session = { id: "pi:ses_bound", harness: "pi", title: "The working session", capabilities: {} }
    stub_show(change_fixture(session: "pi:ses_bound")) do
      BridgeClient.stub(:session, session) do
        BridgeClient.stub(:messages, []) do
          get change_path("fleet", 81)
        end
      end
    end

    assert_response :success
    assert_select "[data-stream-url='/sessions/pi:ses_bound/stream']"
    assert_select "#transcript"
  end

  test "a working change mounts the poll; a reviewed one does not" do
    stub_show(change_fixture(state: "working")) do
      get change_path("fleet", 81)
    end
    assert_select "[data-controller='change-poll'][data-change-poll-state-value='working']"

    stub_show(change_fixture) do
      get change_path("fleet", 81)
    end
    assert_select "[data-controller='change-poll']", 0
  end

  test "status answers the poll with the staleness fields" do
    stub_show(change_fixture) do
      get change_status_path("fleet", 81)
    end

    assert_response :success
    payload = JSON.parse(response.body)
    assert_equal "in_review", payload["state"]
    assert_equal 1, payload["rounds"]
    assert_equal false, payload["deployPending"]
  end

  test "approve redirects with the landing notice" do
    calls = []
    BridgeClient.stub(:approve_change, ->(repo, card) { calls << [ repo, card ]; { state: "landing" } }) do
      post approve_change_path("fleet", 81)
    end

    assert_redirected_to change_path("fleet", 81)
    assert_match(/Landing/, flash[:notice])
    assert_equal [ [ "fleet", "81" ] ], calls
  end

  test "approve surfaces the bridge's refusal, not 'unreachable'" do
    refused = lambda do |*|
      raise BridgeClient::Error.new(
        "skiff bridge unreachable: HTTP 409",
        status: 409, remote_message: "change fleet/81 is working; cannot move to landing"
      )
    end
    BridgeClient.stub(:approve_change, refused) do
      post approve_change_path("fleet", 81)
    end

    assert_redirected_to change_path("fleet", 81)
    assert_match(/change fleet\/81 is working/, flash[:alert])
    refute_match(/unreachable/, flash[:alert])
  end

  test "request_changes requires a note and sends it on" do
    post request_changes_change_path("fleet", 81), params: { note: "  " }
    assert_match(/Type the note first/, flash[:alert])

    calls = []
    BridgeClient.stub(:request_changes, ->(repo, card, note) { calls << [ repo, card, note ]; { state: "working" } }) do
      post request_changes_change_path("fleet", 81), params: { note: "tighten the error copy" }
    end

    assert_redirected_to change_path("fleet", 81)
    assert_match(/the agent is working/, flash[:notice])
    assert_equal [ [ "fleet", "81", "tighten the error copy" ] ], calls
  end

  test "a dead socket reads out as unreachable on show" do
    BridgeClient.stub(:change, ->(*) { raise BridgeClient::Error, "skiff bridge unreachable: down" }) do
      get change_path("fleet", 81)
    end

    assert_response :success
    assert_select ".instrumentation--danger", text: /skiff bridge unreachable/
  end

  test "show renders the deploy preview and the per-service readout" do
    deployed = change_fixture.merge(
      willDeploy: { services: 7 },
      deploy: {
        at: "2026-08-23T12:01:00Z",
        error: nil,
        services: [
          { name: "lighthouse", jobId: "lighthouse-1", status: "started", outcome: { ok: true } },
          { name: "tidepool", jobId: "tidepool-2", status: "started", outcome: { ok: false, message: "build failed" } },
          { name: "sonar", jobId: nil, status: "in_progress", outcome: nil }
        ]
      }
    )
    stub_show(deployed) do
      get change_path("fleet", 81)
    end

    assert_response :success
    assert_select "p.instrumentation", text: /approval will deploy the whole fleet · 7 services/
    assert_select "p.instrumentation", text: /deploy · lighthouse · deployed/
    assert_select "p.instrumentation.instrumentation--danger", text: /deploy · tidepool · failed — build failed/
    assert_select "p.instrumentation", text: /deploy · sonar · already deploying/
  end

  test "show reports a deploy that never triggered" do
    stub_show(
      change_fixture.merge(deploy: { at: "2026-08-23T12:01:00Z", error: "tugboat daemon unreachable", services: [] })
    ) do
      get change_path("fleet", 81)
    end

    assert_response :success
    assert_select "p.instrumentation.instrumentation--danger", text: /deploy not triggered — tugboat daemon unreachable/
  end

  test "the page keeps polling while a shipped change's deploy is in flight" do
    pending = change_fixture(state: "shipped").merge(
      deploy: { at: "x", error: nil, services: [ { name: "lighthouse", jobId: "lighthouse-1", status: "started", outcome: nil } ] }
    )
    stub_show(pending) do
      get change_path("fleet", 81)
    end
    assert_select "[data-controller='change-poll'][data-change-poll-deploy-pending-value='true']"

    done = change_fixture(state: "shipped").merge(
      deploy: { at: "x", error: nil, services: [ { name: "lighthouse", jobId: "lighthouse-1", status: "started", outcome: { ok: true } } ] }
    )
    stub_show(done) do
      get change_path("fleet", 81)
    end
    assert_select "[data-controller='change-poll']", 0
  end
end
