module RunsHelper
  # A timing, in seconds rather than milliseconds — nobody thinks in
  # milliseconds, and a column of four-digit numbers hides the difference
  # between 0.2 and 4 seconds, which is the whole story.
  #
  # Missing data prints as a dash, never a zero, which would read as "instant"
  # rather than "not measured".
  def seconds_figure(milliseconds, degraded: false)
    return tag.span("—", class: "figure--none") if milliseconds.blank?

    value = milliseconds / 1000.0
    text = value < 10 ? format("%.2f", value) : format("%.1f", value)
    tag.span(text, class: degraded ? "figure--bad" : nil)
  end

  # Server effort as a share of what the app can use, not Docker's raw
  # percent-of-one-core (where "513%" is meaningless without knowing the worker
  # count).
  def effort_figure(cpu_pct)
    return tag.span("—", class: "figure--none") if cpu_pct.blank?

    share = (cpu_pct / Analysis.cpu_ceiling_pct * 100).round
    tag.span("#{share}%", class: share >= 85 ? "figure--bad" : nil)
  end

  def rate_figure(value, precision: 1)
    return tag.span("—", class: "figure--none") if value.blank?

    number_with_precision(value, precision: precision)
  end

  def error_figure(count)
    return tag.span("0", class: "figure--none") if count.to_i.zero?

    tag.span(number_with_delimiter(count), class: "figure--bad")
  end

  def request_count_figure(count)
    return tag.span("—", class: "figure--none") if count.blank?

    number_with_delimiter(count)
  end

  # Requests the app never answered, carrying the share that gives the count its
  # scale — 157 is most of a small run and nothing at all in a large one.
  #
  # Zero prints muted rather than red. A run where every request came back should
  # not draw the eye to the figure saying so.
  #
  # Takes the numbers rather than a run, because a run in flight has no row to
  # ask: the same figure is drawn from the runner's running totals while the
  # test is going and from the imported row afterwards.
  def unanswered_figure(count, share = nil)
    return tag.span("—", class: "figure--none") if count.blank?
    return tag.span("0", class: "figure--none") if count.zero?

    text = number_with_delimiter(count)
    # Sub-percent shares get a second digit: "0.0%" reads as none, and none is
    # the one thing this figure is not saying.
    text += " (#{number_to_percentage(share * 100, precision: share < 0.01 ? 2 : 1)})" if share

    tag.span(text, class: "figure--bad")
  end

  def seconds_label(value)
    return "—" if value.blank?

    value < 10 ? "#{format('%.1f', value)} seconds" : "#{value.round} seconds"
  end

  def bytes_in_mb(bytes)
    return "—" if bytes.blank?

    "#{number_with_precision(bytes / 1_048_576.0, precision: 1)} MB"
  end

  # Lever names as a person would say them, not as environment variables.
  LEVER_LABELS = {
    "VUS" => "people at once",
    "USER_POOL" => "distinct accounts used",
    "HOLD" => "how long",
    "ARRIVAL_RATE" => "requests per second sent",
    "ARRIVAL_DURATION" => "how long",
    "SUBSCRIBERS" => "idle listeners",
    "RAMP_STEPS" => "steps",
    "STEP_HOLD" => "time at each step",
    "ROOM_SIZE" => "room size",
    "SESSION_SECONDS" => "session length (seconds)"
  }.freeze

  def lever_label(key)
    LEVER_LABELS[key] || key.downcase.tr("_", " ")
  end

  def run_duration(run)
    seconds = run.level_stats.sum(:window_seconds)
    return "—" if seconds.zero?

    minutes, remainder = seconds.divmod(60)
    minutes.positive? ? "#{minutes}m #{remainder}s" : "#{remainder}s"
  end
end
