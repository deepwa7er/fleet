require "test_helper"

# DW-001 §8 discipline: the stream action is a pure function of the bridge's
# SSE frames, so these tests stub BridgeClient.stream_session to yield
# canned frames and assert on the re-framed turbo-stream markup. Never a
# live server. The frame shapes mirror bridge/lib/stream-registry.js: each
# event carries the already-mapped { info:, parts: } entry, so the action's
# job is translation only.
class StreamTest < ActionDispatch::IntegrationTest
  def frame(event, payload)
    "event: #{event}\ndata: #{JSON.generate(payload)}\n\n"
  end

  def user_message(id, text)
    {
      info: { id: id, role: "user", agent: nil, time: { created: 1 } },
      parts: [ { type: "text", text: text, synthetic: false } ]
    }
  end

  def assistant_message(id, text, completed: true)
    info = { id: id, role: "assistant", agent: "build", time: { created: 1 } }
    info[:time][:completed] = 2 if completed
    { info: info, parts: [ { type: "text", text: text, synthetic: false } ] }
  end

  # The bridge's live overlay: the in-flight assistant message under the
  # fixed "<pending>" id, with no time.completed (see pi-rpc.js).
  def pending_overlay(text)
    {
      info: { id: "<pending>", role: "assistant", agent: "build", time: { created: 1 } },
      parts: [ { type: "text", text: text, synthetic: false } ]
    }
  end

  def stub_stream(frames, &block)
    BridgeClient.stub(:stream_session, ->(_id, &yield_chunks) { frames.each { |f| yield_chunks.call(f) } }, &block)
  end

  def data_lines(body)
    body.split("\n\n").map do |message|
      message.lines.filter_map do |line|
        line.delete_prefix("data: ").chomp if line.start_with?("data: ")
      end.join("\n")
    end
  end

  test "streams the snapshot as transcript, working, and orchestrator replacements" do
    payload = {
      messages: [ user_message("msg_1", "hello"), assistant_message("msg_2", "hi") ],
      pending: { index: 2, entry: pending_overlay("streaming…") },
      working: true,
      orchestrator: { active: true, widget: nil, status: nil }
    }
    stub_stream([ frame("snapshot", payload) ]) do
      get session_stream_path("pi:ses_123")
    end

    assert_response :success
    assert_equal "text/event-stream", response.media_type
    html = data_lines(response.body).join

    assert_includes html, '<turbo-stream action="replace" target="transcript">'
    assert_includes html, '<turbo-stream action="replace" target="session-status">'
    assert_includes html, '<turbo-stream action="replace" target="orchestrator-readout">'
    # The transcript renders the file messages plus the pending overlay at
    # the stable positional index the stream's replaces will target.
    assert_includes html, 'id="message-0"'
    assert_includes html, 'id="message-1"'
    assert_includes html, 'id="message-2"'
    assert_includes html, "streaming…"
    # The overlay's reasoning renders open; the working readout shows the tag.
    assert_includes html, 'class="instrumentation status-tag">working'
    assert_includes html, "orchestrator on"
  end

  test "streams append and replace actions at the entry's index" do
    frames = [
      frame("append", { index: 2, entry: user_message("msg_3", "next question") }),
      frame("replace", { index: 1, entry: assistant_message("msg_2b", "revised answer") })
    ]
    stub_stream(frames) do
      get session_stream_path("pi:ses_123")
    end

    assert_response :success
    html = data_lines(response.body).join
    assert_includes html, '<turbo-stream action="append" target="transcript">'
    assert_includes html, 'id="message-2"'
    assert_includes html, "next question"
    assert_includes html, '<turbo-stream action="replace" target="message-1">'
    assert_includes html, "revised answer"
  end

  test "streams remove, working, and orchestrator actions" do
    frames = [
      frame("remove", { index: 3 }),
      frame("working", { working: false }),
      frame("orchestrator", { orchestrator: { active: false, widget: nil, status: nil } })
    ]
    stub_stream(frames) do
      get session_stream_path("pi:ses_123")
    end

    assert_response :success
    html = data_lines(response.body).join
    assert_includes html, '<turbo-stream action="remove" target="message-3">'
    assert_includes html, '<turbo-stream action="replace" target="session-status">'
    # Idle: no working tag, no abort key.
    refute_includes html, "status-tag"
    refute_includes html, 'action="/sessions/pi:ses_123/abort"'
    assert_includes html, '<turbo-stream action="replace" target="orchestrator-readout">'
    assert_includes html, "orchestrator off"
  end

  test "the working event renders the tag and the abort key" do
    stub_stream([ frame("working", { working: true }) ]) do
      get session_stream_path("pi:ses_123")
    end

    html = data_lines(response.body).join
    assert_includes html, 'class="instrumentation status-tag">working'
    assert_includes html, 'action="/sessions/pi:ses_123/abort"'
  end

  test "preserves the turbo-stream markup across the SSE framing" do
    message = user_message("msg_1", "line one\nline two")
    stub_stream([ frame("append", { index: 0, entry: message }) ]) do
      get session_stream_path("pi:ses_123")
    end

    assert_response :success
    # The HTML's newlines ride as separate data: lines; the browser joins
    # them back with \n, so the turbo-stream document — <br> and all — is
    # byte-identical to what the action rendered.
    html = data_lines(response.body).join
    assert_includes html, 'action="append" target="transcript"'
    assert_includes html, "<br>\nline two"
    assert html.end_with?("</turbo-stream>")
  end

  test "ignores unknown events and ends quietly when the upstream fails" do
    stub_stream([ frame("something_new", {}) ]) do
      get session_stream_path("pi:ses_123")
    end
    assert_response :success
    assert_equal "", response.body

    BridgeClient.stub(:stream_session, ->(*) { raise BridgeClient::Error, "bridge down" }) do
      get session_stream_path("pi:ses_123")
    end
    assert_response :success
    assert_equal "", response.body
  end

  # The bridge's heartbeat reaches the browser as an SSE comment: invisible
  # to EventSource, but the write is what lets a Puma thread notice that
  # the viewer has gone (see the stream action's comment).
  test "forwards each heartbeat as an SSE comment and nothing else" do
    stub_stream([ frame("heartbeat", {}), frame("working", { working: true }), frame("heartbeat", {}) ]) do
      get session_stream_path("pi:ses_123")
    end

    assert_response :success
    messages = response.body.split("\n\n")
    assert_equal 3, messages.size
    assert_equal ": heartbeat", messages[0]
    assert_includes messages[1], 'target="session-status"'
    assert_equal ": heartbeat", messages[2]
  end

  # Once the viewer is gone, ActionController::Live's buffer raises
  # ClientDisconnected from the next write — the heartbeat forward, for an
  # idle session. The action must end quietly, releasing the thread.
  test "a viewer that has gone away ends the stream quietly" do
    gone = lambda do |_id, &yield_chunks|
      yield_chunks.call(frame("heartbeat", {}))
      raise ActionController::Live::ClientDisconnected, "client disconnected"
    end
    BridgeClient.stub(:stream_session, gone) do
      get session_stream_path("pi:ses_123")
    end

    assert_response :success
    assert_equal ": heartbeat\n\n", response.body
  end
end
