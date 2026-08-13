require "test_helper"

# DW-001 §8 discipline: controller tests stub OpencodeClient class methods with
# canned deep-symbol hashes (the shape the client returns), so the suite never
# touches the wire. The fixture shape below is the contract M3/M4 build on:
# a session is { id:, title:, directory:, agent:, model: { id:, providerID:,
# variant: }, time: { created:, updated: }, cost:, tokens:, summary: } with ms
# epoch times, and any of these may be absent or empty.
class SessionsTest < ActionDispatch::IntegrationTest
  def stub_sessions(list, &block)
    OpencodeClient.stub(:sessions, list, &block)
  end

  def session_fixture(id:, title:, directory:, model:, updated:, created:)
    {
      id: id,
      slug: "slug-#{id}",
      title: title,
      directory: directory,
      agent: "builder",
      model: model,
      time: { created: created, updated: updated },
      cost: 0.5,
      tokens: { input: 100, output: 50 },
      summary: { additions: 0, deletions: 0, files: 0 }
    }
  end

  def sessions_fixtures
    [
      session_fixture(
        id: "ses_newest",
        title: "Newest session",
        directory: "/Users/deepwater/code/blog",
        model: { id: "deepseek-v4-flash", providerID: "deepseek", variant: "default" },
        created: 1_780_000_000_000,
        updated: 1_780_000_200_000
      ),
      session_fixture(
        id: "ses_middle",
        title: "Middle session",
        directory: "/Users/deepwater/code",
        model: { id: "deepseek-v4-flash", providerID: "deepseek", variant: "default" },
        created: 1_780_000_000_000,
        updated: 1_780_000_100_000
      ),
      session_fixture(
        id: "ses_oldest",
        title: "Oldest session",
        directory: "/Users/deepwater/code/skiff/app",
        model: nil,
        created: 1_780_000_000_000,
        updated: nil
      ),
      session_fixture(
        id: "ses_untitled",
        title: "",
        directory: "/Users/deepwater/code",
        model: nil,
        created: nil,
        updated: nil
      )
    ]
  end

  test "index renders sessions sorted by last activity descending" do
    stub_sessions(sessions_fixtures) do
      get sessions_path
    end

    assert_response :success
    titles = css_select(".item-title").map(&:text)
    assert_equal [ "Newest session", "Middle session", "Oldest session", "Untitled session" ], titles
    assert_select ".measure > .instrumentation", text: "4 sessions"
    assert_select ".item .instrumentation", text: /code\/blog/
    assert_select "a.item-title[href='/sessions/ses_newest']"
  end

  test "index renders the empty state for an empty list" do
    stub_sessions([]) do
      get sessions_path
    end

    assert_response :success
    assert_select ".list", count: 0
    assert_select ".prose", text: /No sessions yet/
  end

  test "index renders the error state with status 200 when the client fails" do
    OpencodeClient.stub(:sessions, ->(*) { raise OpencodeClient::Error, "boom" }) do
      get sessions_path
    end

    assert_response :success
    assert_select ".instrumentation--danger", text: /opencode server unreachable/
    assert_select ".list", count: 0
  end

  test "create calls create_session and redirects to the new session" do
    OpencodeClient.stub(:create_session, { id: "ses_new", title: "New session" }) do
      post sessions_path
    end

    assert_redirected_to session_path("ses_new")
  end

  test "create redirects to root with a danger flash when the client fails" do
    OpencodeClient.stub(:create_session, ->(*) { raise OpencodeClient::Error, "boom" }) do
      post sessions_path
    end

    assert_redirected_to root_path
    assert_equal "Could not create a session — opencode server unreachable", flash[:alert]
  end

  test "show renders the transcript with the session header" do
    session = {
      id: "ses_123",
      title: "Test session",
      directory: "/Users/deepwater/code/blog",
      model: { id: "deepseek-v4-flash", providerID: "deepseek", variant: "default" },
      time: { created: 1_780_000_000_000, updated: 1_780_000_200_000 }
    }
    messages = [
      {
        info: { id: "msg_1", sessionID: "ses_123", role: "user", parentID: nil,
                agent: nil, model: "deepseek-v4-flash",
                time: { created: 1_780_000_000_000 }, finish: nil, tokens: nil, cost: 0 },
        parts: [
          { type: "text", text: "Fix the header", synthetic: false,
            time: { start: 1_780_000_000_000, end: 1_780_000_000_100 } },
          # Synthetic text is auto-attached context — must render nothing.
          { type: "text", text: "auto-attached context", synthetic: true,
            time: { start: 1_780_000_000_000, end: 1_780_000_000_050 } }
        ]
      },
      {
        info: { id: "msg_2", sessionID: "ses_123", role: "assistant", parentID: "msg_1",
                agent: "build", model: "deepseek-v4-flash",
                time: { created: 1_780_000_000_000, completed: 1_780_000_010_000 },
                finish: "completed", tokens: nil, cost: 0.1 },
        parts: [
          { type: "reasoning", text: "Let me look at the layout first." },
          { type: "tool", callID: "call_1", tool: "grep",
            state: { status: "completed", title: "searching",
                     input: { query: "header" }, output: "lots of output" } },
          # step-start/step-finish are control parts — must render nothing.
          { type: "step-start", time: { start: 1_780_000_010_000 } },
          { type: "step-finish", time: { start: 1_780_000_010_000, end: 1_780_000_015_000 } },
          { type: "file", mime: "text/markdown", filename: "plan.md", url: "opencode://x" },
          { type: "text", text: "```ruby\nputs 1\n```", synthetic: false,
            time: { start: 1_780_000_010_000, end: 1_780_000_015_000 } }
        ]
      }
    ]

    OpencodeClient.stub(:session, session) do
      OpencodeClient.stub(:messages, messages) do
        get session_path("ses_123")
      end
    end

    assert_response :success
    assert_select "h2", text: "Test session"
    assert_select "h2 + p.instrumentation", text: /deepseek-v4-flash · code\/blog/
    assert_select ".message-role", text: "you"
    assert_select ".message-role", text: "opencode · build"
    # The user-jump widget's hook: exactly the user's own block is marked.
    assert_select "section.message[data-mine='true']", count: 1
    assert_select ".prose", text: /Fix the header/
    assert_select ".reasoning summary", text: "reasoning"
    assert_select ".reasoning .prose", text: /Let me look at the layout first/
    # A settled reasoning block renders closed — the user opens it on tap.
    assert_select "details.reasoning[open]", count: 0
    assert_select ".tool-line", text: /grep · completed · searching/
    assert_select ".tool-line", text: /file · plan\.md/
    assert_select "pre code.language-ruby", text: /puts 1/
    assert_select "form.composer textarea[name='message']"
    assert_select "button[type='submit']", text: "Send"
    assert_select "a.back-link[href='#{root_path}']"
    # The rename disclosure: the key, the recessed field pre-filled with the
    # current title, and the save key. It sits outside the poll wrapper.
    assert_select "details.rename summary.button--small", text: "Rename"
    assert_select "form[action='/sessions/ses_123/name'] label", text: "Session name"
    assert_select "form[action='/sessions/ses_123/name'] input[name='name'][value='Test session']"
    assert_select "form[action='/sessions/ses_123/name'] button[type='submit']", text: "Save name"
    assert_select "div[data-controller='stream chat-scroll'] details.rename", count: 0

    # M4: the working readout is a separate element, replaced by the stream's
    # working events. The fixture's newest message is a completed assistant
    # turn, so the session renders idle with no Abort key.
    assert_select "div#session-status"
    assert_select "div#session-status .status-tag", count: 0
    assert_select "form[action='/sessions/ses_123/abort']", count: 0
    # The orchestrator readout lives in its own element, replaced by the
    # stream's orchestrator events.
    assert_select "div#orchestrator-readout"
    # M4: the stream wiring lives on the stable wrapper around the three
    # stream targets (never itself a stream target), so chat-scroll and
    # user-jump survive the transcript replacements that settle every
    # reconnect, and the user-jump widget (hidden until it has two messages
    # to jump between) rides along untouched.
    assert_select "div[data-controller='stream chat-scroll user-jump'][data-stream-url='/sessions/ses_123/stream']"
    assert_select "div[data-controller='stream chat-scroll user-jump'] #session-status"
    assert_select "div[data-controller='stream chat-scroll user-jump'] #orchestrator-readout"
    assert_select "div[data-controller='stream chat-scroll user-jump'] #transcript"
    assert_select "nav.user-jump[hidden] .user-jump-button", count: 2

    assert_no_match(/auto-attached context/, response.body)
    assert_no_match(/step-start/, response.body)
    assert_no_match(/step-finish/, response.body)
  end

  test "show marks a working session with the working tag and the abort key" do
    session = { id: "ses_123", title: "Test session", directory: nil, model: nil, time: nil }
    messages = [
      { info: { id: "msg_1", role: "user", agent: nil, time: { created: 1 } }, parts: [] },
      # Streaming: the newest assistant message has no time.completed yet.
      { info: { id: "msg_2", role: "assistant", agent: "build", time: { created: 1 } },
        parts: [ { type: "text", text: "streaming", synthetic: false } ] }
    ]

    OpencodeClient.stub(:session, session) do
      OpencodeClient.stub(:messages, messages) do
        get session_path("ses_123")
      end
    end

    assert_response :success
    assert_select "div#session-status .status-tag", text: "working"
    assert_select "form[action='/sessions/ses_123/abort']"
    assert_select "form[action='/sessions/ses_123/abort'] button.button--small.button--secondary"
  end

  test "show renders the live overlay with its reasoning open" do
    session = { id: "ses_123", title: "Test session", directory: nil, model: nil, time: nil }
    messages = [
      { info: { id: "msg_1", role: "user", agent: nil, time: { created: 1 } }, parts: [] },
      # The bridge's in-flight assistant message (see pi-rpc.js).
      { info: { id: "<pending>", role: "assistant", agent: "build", time: { created: 1 } },
        parts: [
          { type: "reasoning", text: "thinking out loud" },
          { type: "text", text: "streaming", synthetic: false }
        ] }
    ]

    OpencodeClient.stub(:session, session) do
      OpencodeClient.stub(:messages, messages) do
        get session_path("ses_123")
      end
    end

    assert_response :success
    # The overlay's reasoning renders open so the thinking is visible live.
    assert_select "details.reasoning[open] .prose", text: /thinking out loud/
    assert_select "#transcript .message", count: 2
  end

  test "show renders the orchestrator readout with the recorded mode" do
    session = {
      id: "ses_123", title: "Test session", directory: nil, model: nil, time: nil,
      orchestrator: { active: true }
    }
    messages = [
      { info: { id: "msg_1", role: "user", agent: nil, time: { created: 1 } }, parts: [] }
    ]

    OpencodeClient.stub(:session, session) do
      OpencodeClient.stub(:messages, messages) do
        get session_path("ses_123")
      end
    end

    assert_response :success
    assert_select "div#orchestrator-readout .orchestrator-tag.orchestrator-tag--on", text: "orchestrator on"
    assert_select "form[action='/sessions/ses_123/orchestrator'] button", text: "Turn off"
    # No live process publication, no readout.
    assert_select "pre.orchestrator-widget", count: 0

    # With the extension's live widget present, the readout renders verbatim.
    widget = [
      "◉ orchestrator ⏳ running — Demo · 5s",
      "  ⏳ Step one",
      "  ✓ Step two"
    ]
    OpencodeClient.stub(:session, session.merge(orchestrator: { active: true, widget: widget })) do
      OpencodeClient.stub(:messages, messages) do
        get session_path("ses_123")
      end
    end

    assert_response :success
    assert_select "pre.orchestrator-widget", text: widget.join("\n")

    OpencodeClient.stub(:session, session.merge(orchestrator: { active: false })) do
      OpencodeClient.stub(:messages, messages) do
        get session_path("ses_123")
      end
    end

    assert_response :success
    assert_select "div#orchestrator-readout .orchestrator-tag", text: "orchestrator off"
    assert_select ".orchestrator-tag--on", count: 0
    assert_select "form[action='/sessions/ses_123/orchestrator'] button", text: "Turn on"
    assert_select "pre.orchestrator-widget", count: 0
  end

  test "rename calls rename_session with the trimmed name and redirects back" do
    OpencodeClient.stub(:rename_session, ->(id, name) {
      assert_equal "ses_123", id
      assert_equal "Better name", name
    }) do
      post rename_session_path("ses_123"), params: { name: "  Better name  " }
    end

    assert_redirected_to session_path("ses_123")
  end

  test "rename ignores a blank name without calling the client" do
    OpencodeClient.stub(:rename_session, ->(*) { flunk "rename_session must not be called for a blank name" }) do
      post rename_session_path("ses_123"), params: { name: "   " }
    end

    assert_redirected_to session_path("ses_123")
  end

  test "rename redirects with a danger flash when the client fails" do
    OpencodeClient.stub(:rename_session, ->(*) { raise OpencodeClient::Error, "boom" }) do
      post rename_session_path("ses_123"), params: { name: "Better name" }
    end

    assert_redirected_to session_path("ses_123")
    assert_equal "Could not rename — opencode server unreachable", flash[:alert]
  end

  test "show renders the error state when the transcript fetch fails" do
    OpencodeClient.stub(:session, { id: "ses_123", title: "Test session", time: nil }) do
      OpencodeClient.stub(:messages, ->(*) { raise OpencodeClient::Error, "boom" }) do
        get session_path("ses_123")
      end
    end

    assert_response :success
    assert_select ".instrumentation--danger", text: /opencode server unreachable/
    assert_select "#transcript", count: 0
    assert_select "form.composer", count: 0
    assert_select "details.rename", count: 0
  end

  test "show renders the error instrumentation when the client fails" do
    OpencodeClient.stub(:session, ->(*) { raise OpencodeClient::Error, "boom" }) do
      get session_path("ses_123")
    end

    assert_response :success
    assert_select ".instrumentation--danger", text: /opencode server unreachable/
    assert_select "#transcript", count: 0
    assert_select "form.composer", count: 0
  end
end
