class SessionsController < ApplicationController
  include ActionController::Live

  # DW-001 §8 discipline: the sessions list is the app's first data page, and
  # every client call can fail — the skiff bridge is a separate
  # process. The error paths are therefore first-class: index renders the
  # chrome with a danger instrumentation line (status 200 so the page stays
  # usable), create redirects to root with a danger flash, show renders the
  # error state, and stream closes the SSE connection so the browser's
  # EventSource retries — the same bounded degradation the poller used.
  #
  # M4 (revised): the session view streams, it does not poll. The browser
  # opens one EventSource per page (stream_controller.js); this controller
  # proxies the bridge's SSE stream and translates each bridge event
  # (snapshot/append/replace/remove/working/orchestrator) into the same
  # turbo-streams the poller used to emit. The translation is mechanical —
  # the bridge decides what changed, so nothing here diffs or fingerprints —
  # and the view's rendering code (partials, controllers) is unchanged.

  # The wire session ids are harness-qualified ("pi:…", "muse:…",
  # "opencode:…"); the bridge names the valid harnesses and create validates
  # against this list before calling out.
  HARNESSES = %w[pi muse opencode].freeze

  def index
    payload = BridgeClient.sessions
    @sessions = payload[:sessions].sort_by { |session| last_activity(session) }.reverse
    # Per-harness failures (opencode serve down, say): the rest of the list
    # still renders, with the gap named instead of silently missing.
    @harness_errors = payload[:errors] || {}
  rescue BridgeClient::Error
    @sessions = []
    @harness_errors = {}
    @error = "skiff bridge unreachable — start it and reload"
  end

  def show
    @session = BridgeClient.session(params[:id])
    @messages = BridgeClient.messages(params[:id])
    # First-paint working state, from the messages alone: a turn in flight
    # leaves the newest assistant message without time.completed. The stream
    # takes over the moment it connects — its snapshot and working events
    # are authoritative.
    @working = session_working?(@messages)
    # The orchestrator readout: the mode the orchestrator extension last
    # recorded in the session file, plus the extension's live widget (plan
    # and worker progress) as captured by the bridge from the live process's
    # fire-and-forget publications. The widget is present only while the
    # extension has one published.
    orchestrator = @session[:orchestrator] || {}
    @orchestrator_active = orchestrator[:active] || false
    @orchestrator_widget = orchestrator[:widget]
    # The model picker's options, only where the harness can switch models.
    # A failed fetch hides the picker rather than failing the page — the
    # transcript is the page's job; the picker is an extra.
    @models = models_for(@session)
  rescue BridgeClient::Error
    @error = "skiff bridge unreachable — start it and reload"
  end

  def create
    harness = params[:harness].to_s
    unless HARNESSES.include?(harness)
      return redirect_to root_path, alert: "Unknown harness — pick pi, muse, or opencode"
    end

    session = BridgeClient.create_session(harness: harness, title: "New session")
    redirect_to session_path(session[:id])
  rescue BridgeClient::Error
    redirect_to root_path, alert: "Could not create a session — skiff bridge unreachable"
  end

  # The streaming proxy. Each bridge SSE frame is translated into turbo-stream
  # HTML and re-framed as one SSE `data:` message; the view renders it with
  # Turbo.renderStreamMessage. The upstream read has no timeout — the bridge
  # owns liveness — and when the bridge or the browser goes away, the action
  # ends and EventSource reconnects (the next snapshot converges the DOM).
  def stream
    response.headers["Content-Type"] = "text/event-stream"
    response.headers["Cache-Control"] = "no-cache"
    response.headers["X-Accel-Buffering"] = "no"
    buffer = +""
    BridgeClient.stream_session(params[:id]) do |chunk|
      buffer << chunk
      while (frame = extract_frame!(buffer))
        actions = translate_stream_event(frame)
        next if actions.blank?

        write_stream_events(actions)
      end
    end
  rescue BridgeClient::Error
    # Upstream unreachable or dropped: end the stream quietly; the browser's
    # EventSource retries and the bridge answers with a snapshot on success.
  rescue ActionController::Live::ClientDisconnected
    # The page went away; closing the upstream read is the teardown.
  ensure
    response.stream.close
  end

  def abort
    BridgeClient.abort(params[:id])
    redirect_to session_path(params[:id])
  rescue BridgeClient::Error
    redirect_to session_path(params[:id]), alert: "Could not abort — skiff bridge unreachable"
  end

  # DW-001 §8 discipline: toggling orchestrator mode is another client call
  # that can fail — the toggle drives the session's live pi process. The
  # failure path redirects back with a danger flash, like abort.
  def orchestrator
    on = params[:on] == "1"
    BridgeClient.orchestrator(params[:id], on)
    redirect_to session_path(params[:id])
  rescue BridgeClient::Error
    redirect_to session_path(params[:id]), alert: "Could not toggle orchestrator — skiff bridge unreachable"
  end

  # DW-001 §8 discipline: switching the model is another client call that can
  # fail. The failure path redirects back with a danger flash, like abort;
  # the bridge validates the model itself (a stale picker posts a model pi no
  # longer knows and gets a loud 400).
  def model
    provider = params[:provider].to_s.strip
    model_id = params[:model].to_s.strip
    return redirect_to session_path(params[:id]) if provider.blank? || model_id.blank?

    BridgeClient.set_model(params[:id], provider: provider, model: model_id)
    redirect_to session_path(params[:id])
  rescue BridgeClient::Error
    redirect_to session_path(params[:id]), alert: "Could not switch model — skiff bridge unreachable"
  end

  # DW-001 §8 discipline: renaming is another client call that can fail — the
  # rename drives the session's live pi process. The failure path redirects
  # back with a danger flash, like abort; blank input never reaches the
  # client (the disclosure form always carries the current title).
  def rename
    name = params[:name].to_s.strip
    return redirect_to session_path(params[:id]) if name.blank?

    BridgeClient.rename_session(params[:id], name)
    redirect_to session_path(params[:id])
  rescue BridgeClient::Error
    redirect_to session_path(params[:id]), alert: "Could not rename — skiff bridge unreachable"
  end

  private

  # Working state for first paint: a turn still in flight leaves the newest
  # assistant message without info.time.completed.
  def session_working?(messages)
    newest = messages.last
    newest && newest[:info][:role] == "assistant" && !newest[:info].dig(:time, :completed)
  end

  # --- stream translation ----------------------------------------------------
  #
  # The bridge's events and their turbo-stream translations (see the
  # registry's protocol comment in bridge/lib/stream-registry.js):
  #   snapshot     -> replace #transcript (messages + pending overlay), the
  #                   working readout, and the orchestrator readout. Sent on
  #                   every connect and after a compaction reset; replacing
  #                   wholesale is what makes reconnection convergent.
  #   append       -> one message appended to #transcript
  #   replace      -> one message replaced at its stable index (overlay
  #                   growth, overlay resolution, tool-result folds)
  #   remove       -> one message removed (aborted run)
  #   working      -> the working readout (tag + abort key)
  #   orchestrator -> the orchestrator readout (mode tag, toggle, widget)

  def translate_stream_event(frame)
    event, payload = parse_frame(frame)
    case event
    when "snapshot" then snapshot_actions(payload)
    when "append" then [ append_action(payload) ]
    when "replace" then [ replace_action(payload) ]
    when "remove" then [ turbo_stream.remove("message-#{payload[:index]}") ]
    when "working" then [ status_action(payload[:working]) ]
    when "orchestrator" then [ orchestrator_action(payload[:orchestrator]) ]
    else [] # unknown events are ignored; the next snapshot converges
    end
  end

  # The author label names the session's harness; the wire id carries it.
  def stream_harness
    params[:id].to_s.split(":").first
  end

  def snapshot_actions(payload)
    entries = Array(payload[:messages])
    # The pending overlay is the in-flight assistant message, always the last
    # bubble (see the registry); the transcript partial renders it like any
    # entry, with the stable positional id the stream's replaces target.
    if (pending = payload[:pending])
      entries = entries + [ pending[:entry] ]
    end
    [
      turbo_stream.replace("transcript", partial: "sessions/transcript",
                                         locals: { messages: entries, harness: stream_harness }),
      status_action(payload[:working]),
      orchestrator_action(payload[:orchestrator])
    ]
  end

  # The message partial renders at the entry's chronological index, so the
  # id="message-N" anchors stay stable as the transcript grows.
  def append_action(payload)
    turbo_stream.append("transcript",
                        partial: "sessions/message",
                        locals: { message: payload[:entry][:info], parts: payload[:entry][:parts],
                                  message_index: payload[:index], harness: stream_harness })
  end

  def replace_action(payload)
    turbo_stream.replace("message-#{payload[:index]}",
                         partial: "sessions/message",
                         locals: { message: payload[:entry][:info], parts: payload[:entry][:parts],
                                   message_index: payload[:index], harness: stream_harness })
  end

  def status_action(working)
    turbo_stream.replace("session-status",
                         partial: "sessions/status",
                         locals: { working: working, session_id: params[:id] })
  end

  def orchestrator_action(orchestrator)
    turbo_stream.replace("orchestrator-readout",
                         partial: "sessions/orchestrator_readout",
                         locals: { orchestrator_active: orchestrator[:active] || false,
                                   orchestrator_widget: orchestrator[:widget],
                                   session_id: params[:id] })
  end

  # One SSE frame from the bridge: `event:` line, `data:` line(s), blank
  # line. Frames split across chunks stay in the buffer until the blank line
  # lands. Returns [ event_name, deep-symbolized payload ].
  def extract_frame!(buffer)
    return nil unless (index = buffer.index("\n\n"))

    frame = buffer.slice!(0, index + 2)
    frame.strip.presence
  end

  def parse_frame(frame)
    event = "message"
    data_lines = []
    frame.each_line do |line|
      if line.start_with?("event: ")
        event = line.delete_prefix("event: ").strip
      elsif line.start_with?("data: ")
        data_lines << line.delete_prefix("data: ").chomp
      end
    end
    return [ event, nil ] if data_lines.empty?

    [ event, JSON.parse(data_lines.join("\n")).deep_symbolize_keys ]
  end

  # Render the translated actions and re-frame them as one SSE data message.
  # Newlines in the HTML become separate data: lines — the browser joins them
  # back with \n, so <pre> content survives the framing intact.
  def write_stream_events(actions)
    html = render_to_string(turbo_stream: actions.reduce(:+))
    html.lines.each { |line| response.stream.write("data: #{line.chomp}\n") }
    response.stream.write("\n")
  end

  # The picker's options for a session whose harness switches models; nil
  # (picker hidden) for other harnesses or when the list cannot be fetched.
  def models_for(session)
    return nil unless session.dig(:capabilities, :model)

    BridgeClient.models(session[:harness])
  rescue BridgeClient::Error
    nil
  end

  # Sort key: last activity is time.updated, falling back to time.created,
  # then 0 so a session missing both sorts last.
  def last_activity(session)
    time = session[:time]
    updated = time && time[:updated]
    created = time && time[:created]
    updated || created || 0
  end
end
