require "test_helper"

# DW-001 §8 discipline: the client is the only code that touches the wire, so
# its tests stub every HTTP call with WebMock and assert the URL path, the
# basic-auth header, and the JSON body. Never a live server.
class OpencodeClientTest < ActiveSupport::TestCase
  BASE_URL = "http://127.0.0.1:4120"
  PASSWORD = "test-password"
  SESSION_ID = "ses_test"

  setup do
    @original_url = ENV["OPENCODE_SERVER_URL"]
    @original_password = ENV["OPENCODE_SERVER_PASSWORD"]
    ENV["OPENCODE_SERVER_URL"] = BASE_URL
    ENV["OPENCODE_SERVER_PASSWORD"] = PASSWORD
  end

  teardown do
    ENV["OPENCODE_SERVER_URL"] = @original_url
    ENV["OPENCODE_SERVER_PASSWORD"] = @original_password
  end

  test "health hits /global/health with basic auth and parses deep symbols" do
    stub_request(:get, "#{BASE_URL}/global/health")
      .with(basic_auth: [ "opencode", PASSWORD ])
      .to_return(status: 200, body: '{"healthy":true,"version":"1.18.15"}')

    assert_equal({ healthy: true, version: "1.18.15" }, OpencodeClient.health)
  end

  test "sessions hits /session and returns the array" do
    body = '[{"id":"ses_1","time":{"created":1780000000000,"updated":1780000001000}}]'
    stub_request(:get, "#{BASE_URL}/session")
      .with(basic_auth: [ "opencode", PASSWORD ])
      .to_return(status: 200, body: body)

    sessions = OpencodeClient.sessions
    assert_equal 1, sessions.length
    assert_equal "ses_1", sessions.first[:id]
    assert_equal 1780000001000, sessions.first[:time][:updated]
  end

  test "session(id) hits /session/:id" do
    stub_request(:get, "#{BASE_URL}/session/#{SESSION_ID}")
      .with(basic_auth: [ "opencode", PASSWORD ])
      .to_return(status: 200, body: '{"id":"ses_test"}')

    assert_equal({ id: "ses_test" }, OpencodeClient.session(SESSION_ID))
  end

  test "messages(id) hits /session/:id/message and returns {info, parts} entries" do
    body = '[{"info":{"id":"msg_1"},"parts":[{"type":"text","text":"hello"}]}]'
    stub_request(:get, "#{BASE_URL}/session/#{SESSION_ID}/message")
      .with(basic_auth: [ "opencode", PASSWORD ])
      .to_return(status: 200, body: body)

    messages = OpencodeClient.messages(SESSION_ID)
    assert_equal "msg_1", messages.first[:info][:id]
    assert_equal "hello", messages.first[:parts].first[:text]
  end

  test "prompt_async posts the text part and returns nil on 204" do
    stub_request(:post, "#{BASE_URL}/session/#{SESSION_ID}/prompt_async")
      .with(
        basic_auth: [ "opencode", PASSWORD ],
        body: '{"parts":[{"type":"text","text":"hello"}]}'
      )
      .to_return(status: 204, body: "")

    assert_nil OpencodeClient.prompt_async(SESSION_ID, "hello")
  end

  test "abort posts to /session/:id/abort and parses the true response" do
    stub_request(:post, "#{BASE_URL}/session/#{SESSION_ID}/abort")
      .with(basic_auth: [ "opencode", PASSWORD ])
      .to_return(status: 200, body: "true")

    assert_equal true, OpencodeClient.abort(SESSION_ID)
  end

  test "create_session posts the title and returns the created session" do
    stub_request(:post, "#{BASE_URL}/session")
      .with(basic_auth: [ "opencode", PASSWORD ], body: '{"title":"New session"}')
      .to_return(status: 200, body: '{"id":"ses_new","title":"New session"}')

    session = OpencodeClient.create_session(title: "New session")
    assert_equal "ses_new", session[:id]
  end

  test "create_session with no title posts an empty object" do
    stub_request(:post, "#{BASE_URL}/session")
      .with(basic_auth: [ "opencode", PASSWORD ], body: "{}")
      .to_return(status: 200, body: '{"id":"ses_new"}')

    assert_equal "ses_new", OpencodeClient.create_session[:id]
  end

  test "stream_session yields the body chunks of the SSE stream" do
    stub_request(:get, "#{BASE_URL}/session/ses_1/stream")
      .with(basic_auth: [ "opencode", PASSWORD ])
      .to_return(status: 200, body: "event: snapshot\ndata: {\"messages\":[]}\n\n")

    chunks = []
    OpencodeClient.stream_session("ses_1") { |chunk| chunks << chunk }
    assert chunks.any? { |chunk| chunk.include?("snapshot") }
  end

  test "stream_session maps an upstream failure to OpencodeClient::Error" do
    stub_request(:get, "#{BASE_URL}/session/ses_1/stream").to_raise(Errno::ECONNREFUSED)

    error = assert_raises(OpencodeClient::Error) { OpencodeClient.stream_session("ses_1") { |_chunk| } }
    assert_match(/opencode server unreachable/, error.message)
  end

  test "reads the password from ENV at call time" do
    ENV["OPENCODE_SERVER_PASSWORD"] = "changed-password"
    stub_request(:get, "#{BASE_URL}/session")
      .with(basic_auth: [ "opencode", "changed-password" ])
      .to_return(status: 200, body: "[]")

    assert_equal [], OpencodeClient.sessions
  end

  test "a refused connection maps to OpencodeClient::Error" do
    stub_request(:get, "#{BASE_URL}/session").to_raise(Errno::ECONNREFUSED)

    error = assert_raises(OpencodeClient::Error) { OpencodeClient.sessions }
    assert_match(/opencode server unreachable/, error.message)
  end

  test "a 401 maps to OpencodeClient::Error with the status" do
    stub_request(:get, "#{BASE_URL}/session").to_return(status: 401, body: "")

    error = assert_raises(OpencodeClient::Error) { OpencodeClient.sessions }
    assert_match(/opencode server unreachable: HTTP 401/, error.message)
  end

  test "a 500 maps to OpencodeClient::Error with a body snippet" do
    stub_request(:get, "#{BASE_URL}/session").to_return(status: 500, body: "Internal Server Error")

    error = assert_raises(OpencodeClient::Error) { OpencodeClient.sessions }
    assert_match(/HTTP 500/, error.message)
    assert_match(/Internal Server Error/, error.message)
  end

  test "an unparseable body maps to OpencodeClient::Error" do
    stub_request(:get, "#{BASE_URL}/session").to_return(status: 200, body: "<html>not json</html>")

    error = assert_raises(OpencodeClient::Error) { OpencodeClient.sessions }
    assert_match(/opencode server unreachable/, error.message)
  end
end
