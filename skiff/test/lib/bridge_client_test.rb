require "test_helper"

# DW-001 §8 discipline: the client is the only code that touches the wire, so
# its tests stub every HTTP call with WebMock and assert the URL path, the
# basic-auth header, and the JSON body. Never a live server.
class BridgeClientTest < ActiveSupport::TestCase
  BASE_URL = "http://127.0.0.1:4120"
  PASSWORD = "test-password"
  SESSION_ID = "pi:ses_test"

  setup do
    @original_url = ENV["SKIFF_BRIDGE_URL"]
    @original_password = ENV["SKIFF_BRIDGE_PASSWORD"]
    ENV["SKIFF_BRIDGE_URL"] = BASE_URL
    ENV["SKIFF_BRIDGE_PASSWORD"] = PASSWORD
  end

  teardown do
    ENV["SKIFF_BRIDGE_URL"] = @original_url
    ENV["SKIFF_BRIDGE_PASSWORD"] = @original_password
  end

  test "health hits /global/health with basic auth and parses deep symbols" do
    stub_request(:get, "#{BASE_URL}/global/health")
      .with(basic_auth: [ "skiff", PASSWORD ])
      .to_return(status: 200, body: '{"status":"ok"}')

    assert_equal({ status: "ok" }, BridgeClient.health)
  end

  test "sessions hits /session and returns the sessions-and-errors object" do
    body = '{"sessions":[{"id":"pi:s1","harness":"pi","time":{"created":1780000000000,"updated":1780000001000}}],' \
           '"errors":{"opencode":"opencode serve unreachable: ECONNREFUSED"}}'
    stub_request(:get, "#{BASE_URL}/session")
      .with(basic_auth: [ "skiff", PASSWORD ])
      .to_return(status: 200, body: body)

    payload = BridgeClient.sessions
    assert_equal 1, payload[:sessions].length
    assert_equal "pi:s1", payload[:sessions].first[:id]
    assert_equal 1780000001000, payload[:sessions].first[:time][:updated]
    assert_match(/unreachable/, payload[:errors][:opencode])
  end

  test "session(id) hits /session/:id with the harness-qualified id" do
    stub_request(:get, "#{BASE_URL}/session/#{SESSION_ID}")
      .with(basic_auth: [ "skiff", PASSWORD ])
      .to_return(status: 200, body: '{"id":"pi:ses_test","harness":"pi"}')

    assert_equal({ id: "pi:ses_test", harness: "pi" }, BridgeClient.session(SESSION_ID))
  end

  test "messages(id) hits /session/:id/message and returns {info, parts} entries" do
    body = '[{"info":{"id":"msg_1"},"parts":[{"type":"text","text":"hello"}]}]'
    stub_request(:get, "#{BASE_URL}/session/#{SESSION_ID}/message")
      .with(basic_auth: [ "skiff", PASSWORD ])
      .to_return(status: 200, body: body)

    messages = BridgeClient.messages(SESSION_ID)
    assert_equal "msg_1", messages.first[:info][:id]
    assert_equal "hello", messages.first[:parts].first[:text]
  end

  test "prompt_async posts the text part and returns nil on 204" do
    stub_request(:post, "#{BASE_URL}/session/#{SESSION_ID}/prompt_async")
      .with(
        basic_auth: [ "skiff", PASSWORD ],
        body: '{"parts":[{"type":"text","text":"hello"}]}'
      )
      .to_return(status: 204, body: "")

    assert_nil BridgeClient.prompt_async(SESSION_ID, "hello")
  end

  test "abort posts to /session/:id/abort and returns nil on 204" do
    stub_request(:post, "#{BASE_URL}/session/#{SESSION_ID}/abort")
      .with(basic_auth: [ "skiff", PASSWORD ])
      .to_return(status: 204, body: "")

    assert_nil BridgeClient.abort(SESSION_ID)
  end

  test "create_session posts the harness and title and returns the created id" do
    stub_request(:post, "#{BASE_URL}/session")
      .with(basic_auth: [ "skiff", PASSWORD ], body: '{"harness":"muse","title":"New session"}')
      .to_return(status: 201, body: '{"id":"muse:2c0ffee0-0000-4000-8000-000000000001"}')

    session = BridgeClient.create_session(harness: "muse", title: "New session")
    assert_equal "muse:2c0ffee0-0000-4000-8000-000000000001", session[:id]
  end

  test "create_session with no title posts the harness alone" do
    stub_request(:post, "#{BASE_URL}/session")
      .with(basic_auth: [ "skiff", PASSWORD ], body: '{"harness":"pi"}')
      .to_return(status: 201, body: '{"id":"pi:ses_new"}')

    assert_equal "pi:ses_new", BridgeClient.create_session(harness: "pi")[:id]
  end

  test "models hits /harness/:name/models and returns the list" do
    stub_request(:get, "#{BASE_URL}/harness/pi/models")
      .with(basic_auth: [ "skiff", PASSWORD ])
      .to_return(status: 200, body: '[{"provider":"deepseek","id":"deepseek-v4-flash"}]')

    assert_equal [ { provider: "deepseek", id: "deepseek-v4-flash" } ], BridgeClient.models("pi")
  end

  test "set_model posts the provider and model id" do
    stub_request(:post, "#{BASE_URL}/session/#{SESSION_ID}/model")
      .with(basic_auth: [ "skiff", PASSWORD ], body: '{"provider":"deepseek","id":"deepseek-v4-pro"}')
      .to_return(status: 200, body: '{"ok":true}')

    assert_equal({ ok: true }, BridgeClient.set_model(SESSION_ID, provider: "deepseek", model: "deepseek-v4-pro"))
  end

  test "stream_session yields the body chunks of the SSE stream" do
    stub_request(:get, "#{BASE_URL}/session/pi:s1/stream")
      .with(basic_auth: [ "skiff", PASSWORD ])
      .to_return(status: 200, body: "event: snapshot\ndata: {\"messages\":[]}\n\n")

    chunks = []
    BridgeClient.stream_session("pi:s1") { |chunk| chunks << chunk }
    assert chunks.any? { |chunk| chunk.include?("snapshot") }
  end

  test "stream_session maps an upstream failure to BridgeClient::Error" do
    stub_request(:get, "#{BASE_URL}/session/pi:s1/stream").to_raise(Errno::ECONNREFUSED)

    error = assert_raises(BridgeClient::Error) { BridgeClient.stream_session("pi:s1") { |_chunk| } }
    assert_match(/skiff bridge unreachable/, error.message)
  end

  # The stream read is bounded so a bridge that stalls without closing the
  # socket cannot hold a Puma thread forever; the bound is sized to the
  # bridge's heartbeat so an idle-but-live session never trips it.
  test "stream_session times out a bridge that goes silent past the heartbeat window" do
    stub_request(:get, "#{BASE_URL}/session/pi:s1/stream").to_timeout

    error = assert_raises(BridgeClient::Error) { BridgeClient.stream_session("pi:s1") { |_chunk| } }
    assert_match(/skiff bridge unreachable/, error.message)
    assert_operator BridgeClient::STREAM_READ_TIMEOUT, :>=, BridgeClient::STREAM_HEARTBEAT_INTERVAL * 2
  end

  test "reads the password from ENV at call time" do
    ENV["SKIFF_BRIDGE_PASSWORD"] = "changed-password"
    stub_request(:get, "#{BASE_URL}/session")
      .with(basic_auth: [ "skiff", "changed-password" ])
      .to_return(status: 200, body: '{"sessions":[],"errors":{}}')

    assert_equal({ sessions: [], errors: {} }, BridgeClient.sessions)
  end

  test "a refused connection maps to BridgeClient::Error" do
    stub_request(:get, "#{BASE_URL}/session").to_raise(Errno::ECONNREFUSED)

    error = assert_raises(BridgeClient::Error) { BridgeClient.sessions }
    assert_match(/skiff bridge unreachable/, error.message)
  end

  test "a 401 maps to BridgeClient::Error with the status" do
    stub_request(:get, "#{BASE_URL}/session").to_return(status: 401, body: "")

    error = assert_raises(BridgeClient::Error) { BridgeClient.sessions }
    assert_match(/skiff bridge unreachable: HTTP 401/, error.message)
  end

  test "a 500 maps to BridgeClient::Error with a body snippet" do
    stub_request(:get, "#{BASE_URL}/session").to_return(status: 500, body: "Internal Server Error")

    error = assert_raises(BridgeClient::Error) { BridgeClient.sessions }
    assert_match(/HTTP 500/, error.message)
    assert_match(/Internal Server Error/, error.message)
  end

  test "an unparseable body maps to BridgeClient::Error" do
    stub_request(:get, "#{BASE_URL}/session").to_return(status: 200, body: "<html>not json</html>")

    error = assert_raises(BridgeClient::Error) { BridgeClient.sessions }
    assert_match(/skiff bridge unreachable/, error.message)
  end

  # ---- DW-002: the change object ----

  test "changes hits /change" do
    stub_request(:get, "#{BASE_URL}/change")
      .with(basic_auth: [ "skiff", PASSWORD ])
      .to_return(status: 200, body: '{"changes":[{"repo":"fleet","card":81,"state":"in_review"}]}')

    assert_equal "in_review", BridgeClient.changes[:changes].first[:state]
  end

  test "change and change_diff address the repo-and-card routes" do
    stub_request(:get, "#{BASE_URL}/change/fleet/81")
      .with(basic_auth: [ "skiff", PASSWORD ])
      .to_return(status: 200, body: '{"repo":"fleet","card":81,"rounds":[]}')
    stub_request(:get, "#{BASE_URL}/change/fleet/81/diff")
      .to_return(status: 200, body: '{"diff":"cumulative"}')
    stub_request(:get, "#{BASE_URL}/change/fleet/81/diff/2")
      .to_return(status: 200, body: '{"diff":"round two"}')

    assert_equal 81, BridgeClient.change("fleet", 81)[:card]
    assert_equal "cumulative", BridgeClient.change_diff("fleet", 81)[:diff]
    assert_equal "round two", BridgeClient.change_diff("fleet", 81, round: 2)[:diff]
  end

  test "approve_change posts and returns the landing change" do
    stub_request(:post, "#{BASE_URL}/change/fleet/81/approve")
      .with(basic_auth: [ "skiff", PASSWORD ])
      .to_return(status: 202, body: '{"state":"landing"}')

    assert_equal "landing", BridgeClient.approve_change("fleet", 81)[:state]
  end

  test "request_changes posts the note" do
    stub_request(:post, "#{BASE_URL}/change/fleet/81/request_changes")
      .with(basic_auth: [ "skiff", PASSWORD ], body: '{"note":"tighten it"}')
      .to_return(status: 200, body: '{"state":"working"}')

    assert_equal "working", BridgeClient.request_changes("fleet", 81, "tighten it")[:state]
  end

  test "session_statuses hits /session/status" do
    stub_request(:get, "#{BASE_URL}/session/status")
      .with(basic_auth: [ "skiff", PASSWORD ])
      .to_return(status: 200, body: '{"pi:busy_one":{"type":"busy"}}')

    assert_equal({ type: "busy" }, BridgeClient.session_statuses[:"pi:busy_one"])
  end

  # The review's verbs need to tell a bridge refusal from a dead socket:
  # HTTP failures carry status + the bridge's own error text; transport
  # failures carry neither. The message contract is unchanged either way.
  test "an HTTP failure carries status and the bridge's error text" do
    stub_request(:post, "#{BASE_URL}/change/fleet/81/approve")
      .to_return(status: 409, body: '{"error":"change fleet/81 is working; cannot move to landing"}')

    error = assert_raises(BridgeClient::Error) { BridgeClient.approve_change("fleet", 81) }
    assert_equal 409, error.status
    assert_equal "change fleet/81 is working; cannot move to landing", error.remote_message
    assert_match(/skiff bridge unreachable: HTTP 409/, error.message)
  end

  test "a transport failure carries neither status nor remote message" do
    stub_request(:get, "#{BASE_URL}/change").to_raise(Errno::ECONNREFUSED)

    error = assert_raises(BridgeClient::Error) { BridgeClient.changes }
    assert_nil error.status
    assert_nil error.remote_message
  end
end
