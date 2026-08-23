require "test_helper"

# The root page (DW-002 §6): registers ordered by what needs you. Stubbed
# like every controller test — BridgeClient class methods return canned
# deep-symbol hashes; the assertions pin the register structure and the
# degrade states.
class DeskTest < ActionDispatch::IntegrationTest
  def change_fixture(card:, state:, title: nil, rounds: 1)
    {
      repo: "fleet",
      card: card,
      title: title,
      state: state,
      updatedAt: "2026-08-23T12:00:00Z",
      rounds: Array.new(rounds) { |i| { n: i + 1, author: "agent", annotations: [] } }
    }
  end

  def session_fixture(id:, title:)
    { id: id, harness: "pi", title: title, time: { created: 1_780_000_000_000, updated: 1_780_000_100_000 } }
  end

  def stub_desk(sessions: [], statuses: {}, changes: [], &block)
    BridgeClient.stub(:sessions, { sessions: sessions, errors: {} }) do
      BridgeClient.stub(:session_statuses, statuses) do
        BridgeClient.stub(:changes, { changes: changes }, &block)
      end
    end
  end

  test "root renders the registers ordered by what needs you" do
    stub_desk(
      sessions: [ session_fixture(id: "pi:busy", title: "Busy one"), session_fixture(id: "pi:quiet", title: "Quiet one") ],
      statuses: { "pi:busy": { type: "busy" } },
      changes: [
        change_fixture(card: 81, state: "in_review", title: "pi model picker", rounds: 2),
        change_fixture(card: 82, state: "working"),
        change_fixture(card: 79, state: "shipped")
      ]
    ) do
      get root_path
    end

    assert_response :success
    # needs you leads, and holds the in-review change.
    assert_select ".register", 3
    assert_select ".register:first-of-type .instrumentation", text: /needs you · 1/
    assert_select ".item-title[href='/changes/fleet/81']", text: "pi model picker"
    assert_select ".item .instrumentation", text: /fleet #81 · round 2 · in review/
    # working holds the working change and the busy session; idle the rest.
    assert_select ".register .instrumentation", text: /working · 2/
    assert_select ".register .instrumentation", text: /idle · 2/
    assert_select ".item-title", text: "Quiet one"
  end

  test "an empty needs-you register is stated, not hidden" do
    stub_desk(sessions: [], statuses: {}, changes: []) do
      get root_path
    end

    assert_response :success
    assert_select ".register:first-of-type .instrumentation", text: /needs you · 0/
    assert_select "p", text: "Nothing is waiting on you."
  end

  test "an unreachable bridge is a named state on a page that renders" do
    BridgeClient.stub(:sessions, ->(*) { raise BridgeClient::Error, "skiff bridge unreachable: down" }) do
      get root_path
    end

    assert_response :success
    assert_select ".instrumentation--danger", text: /skiff bridge unreachable/
  end

  test "a broken change subsystem degrades to the session registers" do
    refused = lambda do |*|
      raise BridgeClient::Error.new(
        "skiff bridge unreachable: HTTP 502",
        status: 502, remote_message: "change subsystem unavailable: jj executable not found"
      )
    end
    BridgeClient.stub(:sessions, { sessions: [ session_fixture(id: "pi:quiet", title: "Quiet one") ], errors: {} }) do
      BridgeClient.stub(:session_statuses, {}) do
        BridgeClient.stub(:changes, refused) do
          get root_path
        end
      end
    end

    assert_response :success
    assert_select ".instrumentation--danger", text: /changes unavailable — change subsystem unavailable/
    assert_select ".item-title", text: "Quiet one"
  end
end
