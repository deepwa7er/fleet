module ApplicationHelper
  # DW-001 §5: the readouts under a session title are instrumentation, so they
  # use the relative-time helper and the path/model facts, never prose.
  def session_title(session)
    session[:title].presence || "Untitled session"
  end

  # One instrumentation line per session, facts joined with · — e.g.
  # "2h · code/blog · deepseek-v4-flash". Each fact is skipped cleanly when
  # absent, so a session without a model or with an empty title never renders
  # dangling separators.
  def session_instrumentation(session)
    [
      relative_activity(session[:time]),
      directory_basename(session[:directory]),
      session.dig(:model, :id)
    ].compact.join(" · ")
  end

  # The show-page readout for one session, model first: "model · directory ·
  # activity". Reuses the same per-fact helpers as session_instrumentation but
  # leads with the model, per the M3 spec. Each fact is skipped cleanly when
  # absent, so a session without a model or directory never renders dangling
  # separators.
  def session_detail_instrumentation(session)
    [
      session.dig(:model, :id),
      directory_basename(session[:directory]),
      relative_activity(session[:time])
    ].compact.join(" · ")
  end

  private

  # Last activity is time.updated, falling back to time.created; nil if the
  # session carries neither (the field is skipped, not rendered as a time).
  def relative_activity(time)
    return nil unless time

    timestamp = time[:updated] || time[:created]
    return nil unless timestamp

    time_ago_in_words(Time.at(timestamp / 1000))
  end

  # The directory readout: the last two path segments when there are at least
  # two ("/Users/deepwater/code/blog" → "code/blog"), else the basename.
  def directory_basename(directory)
    return nil if directory.blank?

    segments = directory.delete_prefix("/").split("/")
    if segments.length >= 2
      segments.last(2).join("/")
    else
      File.basename(directory)
    end
  end
end
