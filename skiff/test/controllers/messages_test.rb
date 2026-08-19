require "test_helper"

# DW-001 §8 discipline: the composer posts through the client and can fail —
# blank input never reaches the wire, and failures redirect back with a danger
# flash. Hermetic: BridgeClient is stubbed, never the live server.
class MessagesTest < ActionDispatch::IntegrationTest
  test "create posts the message and redirects to the session" do
    calls = []
    BridgeClient.stub(:prompt_async, ->(id, text) { calls << [ id, text ] }) do
      post session_messages_path("pi:ses_123"), params: { message: "hello" }
    end

    assert_equal [ [ "pi:ses_123", "hello" ] ], calls
    assert_redirected_to session_path("pi:ses_123")
  end

  test "blank messages never reach the client" do
    calls = []
    BridgeClient.stub(:prompt_async, ->(id, text) { calls << [ id, text ] }) do
      post session_messages_path("pi:ses_123"), params: { message: "   " }
    end

    assert_equal [], calls
    assert_redirected_to session_path("pi:ses_123")
  end

  test "a failed send redirects back with a danger flash" do
    BridgeClient.stub(:prompt_async, ->(*) { raise BridgeClient::Error, "boom" }) do
      post session_messages_path("pi:ses_123"), params: { message: "hello" }
    end

    assert_redirected_to session_path("pi:ses_123")
    assert_equal "Could not send — skiff bridge unreachable", flash[:alert]
  end
end
