require "test_helper"

# DW-001 §8 discipline: controller tests stub BridgeClient class methods with
# canned deep-symbol hashes (the shape the client returns), so the suite never
# touches the wire. The fixture shape below is the contract M3/M4 build on:
# a session is { id:, harness:, capabilities:, title:, directory:, model:
# { id: }, time: { created:, updated: } } with ms epoch times and a
# harness-qualified id, and any of these may be absent or empty.
class SessionsTest < ActionDispatch::IntegrationTest
  PI_CAPS = { rename: true, orchestrator: true }.freeze
  MUSE_CAPS = { rename: false, orchestrator: false }.freeze

  def stub_sessions(list, errors: {}, &block)
    BridgeClient.stub(:sessions, { sessions: list, errors: errors }, &block)
  end

  def session_fixture(id:, title:, directory:, model:, updated:, created:, harness: "pi", capabilities: PI_CAPS)
    {
      id: id,
      harness: harness,
      capabilities: capabilities,
      slug: "slug-#{id}",
      title: title,
      directory: directory,
      model: model,
      time: { created: created, updated: updated }
    }
  end

  def sessions_fixtures
    [
      session_fixture(
        id: "pi:ses_newest",
        title: "Newest session",
        directory: "/Users/deepwater/code/blog",
        model: { id: "deepseek-v4-flash", providerID: "deepseek", variant: "default" },
        created: 1_780_000_000_000,
        updated: 1_780_000_200_000
      ),
      session_fixture(
        id: "pi:ses_middle",
        title: "Middle session",
        directory: "/Users/deepwater/code",
        model: { id: "deepseek-v4-flash", providerID: "deepseek", variant: "default" },
        created: 1_780_000_000_000,
        updated: 1_780_000_100_000
      ),
      session_fixture(
        id: "pi:ses_oldest",
        title: "Oldest session",
        directory: "/Users/deepwater/code/skiff/app",
        model: nil,
        created: 1_780_000_000_000,
        updated: nil
      ),
      session_fixture(
        id: "pi:ses_untitled",
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
    assert_select ".item .instrumentation", text: /pi · .*code\/blog/
    assert_select "a.item-title[href='/sessions/pi:ses_newest']"
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
    BridgeClient.stub(:sessions, ->(*) { raise BridgeClient::Error, "boom" }) do
      get sessions_path
    end

    assert_response :success
    assert_select ".instrumentation--danger", text: /skiff bridge unreachable/
    assert_select ".list", count: 0
  end

  test "index names an unreachable harness while the rest still renders" do
    stub_sessions(sessions_fixtures, errors: { opencode: "opencode serve unreachable: ECONNREFUSED" }) do
      get sessions_path
    end

    assert_response :success
    assert_select ".instrumentation--danger", text: /opencode unreachable — its sessions are not listed/
    assert_select ".item-title", count: 4
  end

  test "index offers one create key per harness" do
    stub_sessions([]) do
      get sessions_path
    end

    assert_response :success
    %w[pi muse opencode].each do |harness|
      assert_select ".action-bar-row form input[name='harness'][value='#{harness}']"
    end
  end

  test "create calls create_session with the chosen harness and redirects" do
    calls = []
    BridgeClient.stub(:create_session, ->(harness:, title:) {
      calls << [ harness, title ]
      { id: "muse:2c0ffee0-0000-4000-8000-000000000001" }
    }) do
      post sessions_path, params: { harness: "muse" }
    end

    assert_equal [ [ "muse", "New session" ] ], calls
    assert_redirected_to session_path("muse:2c0ffee0-0000-4000-8000-000000000001")
  end

  test "create rejects an unknown harness without calling the client" do
    BridgeClient.stub(:create_session, ->(*) { flunk "create_session must not be called" }) do
      post sessions_path, params: { harness: "clippy" }
    end

    assert_redirected_to root_path
    assert_equal "Unknown harness — pick pi, muse, or opencode", flash[:alert]
  end

  test "create redirects to root with a danger flash when the client fails" do
    BridgeClient.stub(:create_session, ->(*) { raise BridgeClient::Error, "boom" }) do
      post sessions_path, params: { harness: "pi" }
    end

    assert_redirected_to root_path
    assert_equal "Could not create a session — skiff bridge unreachable", flash[:alert]
  end

  test "show renders the transcript with the session header" do
    session = {
      id: "pi:ses_123",
      harness: "pi",
      capabilities: { rename: true, orchestrator: true },
      title: "Test session",
      directory: "/Users/deepwater/code/blog",
      model: { id: "deepseek-v4-flash" },
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

    BridgeClient.stub(:session, session) do
      BridgeClient.stub(:messages, messages) do
        get session_path("pi:ses_123")
      end
    end

    assert_response :success
    assert_select "h2", text: "Test session"
    assert_select "h2 + p.instrumentation", text: /pi · deepseek-v4-flash · code\/blog/
    assert_select ".message-role", text: "you"
    assert_select ".message-role", text: "pi · build"
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
    assert_select "form[action='/sessions/pi:ses_123/name'] label", text: "Session name"
    assert_select "form[action='/sessions/pi:ses_123/name'] input[name='name'][value='Test session']"
    assert_select "form[action='/sessions/pi:ses_123/name'] button[type='submit']", text: "Save name"
    assert_select "div[data-controller='stream chat-scroll'] details.rename", count: 0

    # M4: the working readout is a separate element, replaced by the stream's
    # working events. The fixture's newest message is a completed assistant
    # turn, so the session renders idle with no Abort key.
    assert_select "div#session-status"
    assert_select "div#session-status .status-tag", count: 0
    assert_select "form[action='/sessions/pi:ses_123/abort']", count: 0
    # The orchestrator readout lives in its own element, replaced by the
    # stream's orchestrator events.
    assert_select "div#orchestrator-readout"
    # M4: the stream wiring lives on the stable wrapper around the three
    # stream targets (never itself a stream target), so chat-scroll and
    # user-jump survive the transcript replacements that settle every
    # reconnect, and the user-jump widget (hidden until it has two messages
    # to jump between) rides along untouched.
    assert_select "div[data-controller='stream chat-scroll user-jump'][data-stream-url='/sessions/pi:ses_123/stream']"
    assert_select "div[data-controller='stream chat-scroll user-jump'] #session-status"
    assert_select "div[data-controller='stream chat-scroll user-jump'] #orchestrator-readout"
    assert_select "div[data-controller='stream chat-scroll user-jump'] #transcript"
    assert_select "nav.user-jump[hidden] .user-jump-button", count: 2

    assert_no_match(/auto-attached context/, response.body)
    assert_no_match(/step-start/, response.body)
    assert_no_match(/step-finish/, response.body)
  end

  test "show marks a working session with the working tag and the abort key" do
    session = { id: "pi:ses_123", harness: "pi", capabilities: { rename: true, orchestrator: true }, title: "Test session", directory: nil, model: nil, time: nil }
    messages = [
      { info: { id: "msg_1", role: "user", agent: nil, time: { created: 1 } }, parts: [] },
      # Streaming: the newest assistant message has no time.completed yet.
      { info: { id: "msg_2", role: "assistant", agent: "build", time: { created: 1 } },
        parts: [ { type: "text", text: "streaming", synthetic: false } ] }
    ]

    BridgeClient.stub(:session, session) do
      BridgeClient.stub(:messages, messages) do
        get session_path("pi:ses_123")
      end
    end

    assert_response :success
    assert_select "div#session-status .status-tag", text: "working"
    assert_select "form[action='/sessions/pi:ses_123/abort']"
    assert_select "form[action='/sessions/pi:ses_123/abort'] button.button--small.button--secondary"
  end

  test "show renders the live overlay with its reasoning open" do
    session = { id: "pi:ses_123", harness: "pi", capabilities: { rename: true, orchestrator: true }, title: "Test session", directory: nil, model: nil, time: nil }
    messages = [
      { info: { id: "msg_1", role: "user", agent: nil, time: { created: 1 } }, parts: [] },
      # The bridge's in-flight assistant message (see pi-rpc.js).
      { info: { id: "<pending>", role: "assistant", agent: "build", time: { created: 1 } },
        parts: [
          { type: "reasoning", text: "thinking out loud" },
          { type: "text", text: "streaming", synthetic: false }
        ] }
    ]

    BridgeClient.stub(:session, session) do
      BridgeClient.stub(:messages, messages) do
        get session_path("pi:ses_123")
      end
    end

    assert_response :success
    # The overlay's reasoning renders open so the thinking is visible live.
    assert_select "details.reasoning[open] .prose", text: /thinking out loud/
    assert_select "#transcript .message", count: 2
  end

  test "show renders the orchestrator readout with the recorded mode" do
    session = {
      id: "pi:ses_123", harness: "pi", capabilities: { rename: true, orchestrator: true },
      title: "Test session", directory: nil, model: nil, time: nil,
      orchestrator: { active: true }
    }
    messages = [
      { info: { id: "msg_1", role: "user", agent: nil, time: { created: 1 } }, parts: [] }
    ]

    BridgeClient.stub(:session, session) do
      BridgeClient.stub(:messages, messages) do
        get session_path("pi:ses_123")
      end
    end

    assert_response :success
    assert_select "div#orchestrator-readout .orchestrator-tag.orchestrator-tag--on", text: "orchestrator on"
    assert_select "form[action='/sessions/pi:ses_123/orchestrator'] button", text: "Turn off"
    # No live process publication, no readout.
    assert_select "pre.orchestrator-widget", count: 0

    # With the extension's live widget present, the readout renders verbatim.
    widget = [
      "◉ orchestrator ⏳ running — Demo · 5s",
      "  ⏳ Step one",
      "  ✓ Step two"
    ]
    BridgeClient.stub(:session, session.merge(orchestrator: { active: true, widget: widget })) do
      BridgeClient.stub(:messages, messages) do
        get session_path("pi:ses_123")
      end
    end

    assert_response :success
    assert_select "pre.orchestrator-widget", text: widget.join("\n")

    BridgeClient.stub(:session, session.merge(orchestrator: { active: false })) do
      BridgeClient.stub(:messages, messages) do
        get session_path("pi:ses_123")
      end
    end

    assert_response :success
    assert_select "div#orchestrator-readout .orchestrator-tag", text: "orchestrator off"
    assert_select ".orchestrator-tag--on", count: 0
    assert_select "form[action='/sessions/pi:ses_123/orchestrator'] button", text: "Turn on"
    assert_select "pre.orchestrator-widget", count: 0
  end

  test "rename calls rename_session with the trimmed name and redirects back" do
    BridgeClient.stub(:rename_session, ->(id, name) {
      assert_equal "pi:ses_123", id
      assert_equal "Better name", name
    }) do
      post rename_session_path("pi:ses_123"), params: { name: "  Better name  " }
    end

    assert_redirected_to session_path("pi:ses_123")
  end

  test "rename ignores a blank name without calling the client" do
    BridgeClient.stub(:rename_session, ->(*) { flunk "rename_session must not be called for a blank name" }) do
      post rename_session_path("pi:ses_123"), params: { name: "   " }
    end

    assert_redirected_to session_path("pi:ses_123")
  end

  test "rename redirects with a danger flash when the client fails" do
    BridgeClient.stub(:rename_session, ->(*) { raise BridgeClient::Error, "boom" }) do
      post rename_session_path("pi:ses_123"), params: { name: "Better name" }
    end

    assert_redirected_to session_path("pi:ses_123")
    assert_equal "Could not rename — skiff bridge unreachable", flash[:alert]
  end

  test "show renders the error state when the transcript fetch fails" do
    BridgeClient.stub(:session, { id: "ses_123", title: "Test session", time: nil }) do
      BridgeClient.stub(:messages, ->(*) { raise BridgeClient::Error, "boom" }) do
        get session_path("pi:ses_123")
      end
    end

    assert_response :success
    assert_select ".instrumentation--danger", text: /skiff bridge unreachable/
    assert_select "#transcript", count: 0
    assert_select "form.composer", count: 0
    assert_select "details.rename", count: 0
  end

  test "show renders the error instrumentation when the client fails" do
    BridgeClient.stub(:session, ->(*) { raise BridgeClient::Error, "boom" }) do
      get session_path("pi:ses_123")
    end

    assert_response :success
    assert_select ".instrumentation--danger", text: /skiff bridge unreachable/
    assert_select "#transcript", count: 0
    assert_select "form.composer", count: 0
  end

  test "show renders the model picker for a harness with the capability" do
    session = {
      id: "pi:ses_123", harness: "pi",
      capabilities: { rename: true, orchestrator: true, model: true },
      title: "Test session", directory: nil,
      model: { id: "deepseek-v4-flash" }, time: nil
    }
    messages = [ { info: { id: "msg_1", role: "user", agent: nil, time: { created: 1 } }, parts: [] } ]
    models = [
      { provider: "deepseek", id: "deepseek-v4-flash" },
      { provider: "deepseek", id: "deepseek-v4-pro" }
    ]

    BridgeClient.stub(:session, session) do
      BridgeClient.stub(:messages, messages) do
        BridgeClient.stub(:models, models) do
          get session_path("pi:ses_123")
        end
      end
    end

    assert_response :success
    # The current model is a readout, not a key; the other is pressable.
    assert_select ".model-picker .instrumentation", text: "deepseek/deepseek-v4-flash · current"
    assert_select "form[action='/sessions/pi:ses_123/model'] input[name='model'][value='deepseek-v4-pro']"
    assert_select "form[action='/sessions/pi:ses_123/model'] button", text: "deepseek/deepseek-v4-pro"
  end

  test "show hides the model picker when the options cannot be fetched" do
    session = {
      id: "pi:ses_123", harness: "pi",
      capabilities: { rename: true, orchestrator: true, model: true },
      title: "Test session", directory: nil, model: nil, time: nil
    }
    messages = [ { info: { id: "msg_1", role: "user", agent: nil, time: { created: 1 } }, parts: [] } ]

    BridgeClient.stub(:session, session) do
      BridgeClient.stub(:messages, messages) do
        BridgeClient.stub(:models, ->(*) { raise BridgeClient::Error, "boom" }) do
          get session_path("pi:ses_123")
        end
      end
    end

    assert_response :success
    assert_select ".model-picker", count: 0
    assert_select "#transcript"
  end

  test "model posts the chosen model and redirects back" do
    calls = []
    BridgeClient.stub(:set_model, ->(id, provider:, model:) { calls << [ id, provider, model ] }) do
      post session_model_path("pi:ses_123"), params: { provider: "deepseek", model: "deepseek-v4-pro" }
    end

    assert_equal [ [ "pi:ses_123", "deepseek", "deepseek-v4-pro" ] ], calls
    assert_redirected_to session_path("pi:ses_123")
  end

  test "model ignores blank params without calling the client" do
    BridgeClient.stub(:set_model, ->(*) { flunk "set_model must not be called" }) do
      post session_model_path("pi:ses_123"), params: { provider: "", model: "x" }
    end

    assert_redirected_to session_path("pi:ses_123")
  end

  test "model redirects with a danger flash when the client fails" do
    BridgeClient.stub(:set_model, ->(*) { raise BridgeClient::Error, "boom" }) do
      post session_model_path("pi:ses_123"), params: { provider: "deepseek", model: "deepseek-v4-pro" }
    end

    assert_redirected_to session_path("pi:ses_123")
    assert_equal "Could not switch model — skiff bridge unreachable", flash[:alert]
  end

  test "show hides rename and orchestrator for a harness without the capabilities" do
    session = {
      id: "muse:2c0ffee0-0000-4000-8000-000000000001",
      harness: "muse",
      capabilities: { rename: false, orchestrator: false },
      title: "lemon-aurora", directory: "/home/deepwater/code/fleet",
      model: { id: "muse-spark-1.2" }, time: { created: 1, updated: 2 }
    }
    messages = [
      { info: { id: "msg_1", role: "user", agent: nil, time: { created: 1 } },
        parts: [ { type: "text", text: "hello muse", synthetic: false } ] },
      { info: { id: "msg_2", role: "assistant", agent: "muse-spark-1.2", time: { created: 1, completed: 2 } },
        parts: [ { type: "text", text: "hello back", synthetic: false } ] }
    ]

    BridgeClient.stub(:session, session) do
      BridgeClient.stub(:messages, messages) do
        get session_path(session[:id])
      end
    end

    assert_response :success
    assert_select "details.rename", count: 0
    assert_select "div#orchestrator-readout", count: 0
    assert_select ".model-picker", count: 0
    # The author label is the session's harness, and the composer names it.
    assert_select ".message-role", text: "muse · muse-spark-1.2"
    assert_select "form.composer textarea[placeholder='Message muse…']"
    assert_select "h2 + p.instrumentation", text: /muse · muse-spark-1\.2/
  end
end
