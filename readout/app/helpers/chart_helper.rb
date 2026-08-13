# Server-rendered inline SVG charts, for the STATIC charts on an imported run.
#
# The live chart during a run is Chart.js instead (see the coping_chart Stimulus
# controller), because it updates a canvas in place — a chart re-rendered as new
# HTML every two seconds flickers, and a live chart has to update constantly.
#
# These are the opposite case: they render once and never change, so a charting
# library would buy nothing. Staying in Ruby keeps them working without
# JavaScript, and their colours come from CSS custom properties via classes,
# which means light and dark mode follow for free — something a canvas cannot do
# without reading the palette back out of the stylesheet by hand.
module ChartHelper
  # Geometry of the bare sparkline (#line_chart).
  CHART_WIDTH = 720
  CHART_HEIGHT = 140
  PADDING_TOP = 12
  PADDING_BOTTOM = 18

  # Geometry of the axed charts (#plotted_chart, #time_series_chart). Taller than
  # the sparkline because these carry ticks and axis labels; the left margin is
  # the room the y ticks need.
  PLOT_WIDTH = 720
  PLOT_HEIGHT = 250
  PLOT_LEFT = 52
  PLOT_RIGHT = 16
  PLOT_TOP = 16
  PLOT_BOTTOM = 42

  # A filled line chart over a series of [x_value, y_value] pairs.
  #
  # `threshold` draws a dashed reference line (used for full CPU saturation of
  # the host), and is skipped when it would fall outside the plotted range —
  # a reference line pinned to the top of every chart teaches the reader
  # nothing.
  def line_chart(points, threshold: nil, label: nil, y_max: nil)
    return content_tag(:p, "No samples recorded.", class: "muted") if points.blank?

    xs = points.map(&:first)
    ys = points.map(&:last)

    x_min = xs.min
    x_span = [ xs.max - x_min, 1 ].max

    # Always anchor at zero: a line chart of a rate whose baseline is hidden
    # exaggerates variation.
    top = [ y_max, ys.max, threshold ].compact.max.to_f
    top = 1.0 if top <= 0

    plot_height = CHART_HEIGHT - PADDING_TOP - PADDING_BOTTOM

    coords = points.map do |x, y|
      px = ((x - x_min).to_f / x_span) * CHART_WIDTH
      py = PADDING_TOP + plot_height - (y.to_f / top * plot_height)
      [ px.round(2), py.round(2) ]
    end

    line = coords.map { |px, py| "#{px},#{py}" }.join(" ")
    baseline = PADDING_TOP + plot_height
    area = "#{coords.first.first},#{baseline} #{line} #{coords.last.first},#{baseline}"

    tag.svg(
      class: "chart",
      viewBox: "0 0 #{CHART_WIDTH} #{CHART_HEIGHT}",
      preserveAspectRatio: "none",
      role: "img",
      "aria-label": label || "chart"
    ) do
      parts = []
      parts << tag.polygon(points: area, class: "chart-area")
      parts << tag.polyline(points: line, class: "chart-line")

      if threshold && threshold <= top
        ty = (PADDING_TOP + plot_height - (threshold.to_f / top * plot_height)).round(2)
        parts << tag.line(x1: 0, y1: ty, x2: CHART_WIDTH, y2: ty, class: "chart-threshold")
      end

      if label
        parts << tag.text(label, x: 0, y: CHART_HEIGHT - 4, class: "chart-label")
      end

      parts << tag.text(
        format_axis_value(top), x: 0, y: PADDING_TOP - 3, class: "chart-label"
      )

      safe_join(parts)
    end
  end

  # Requests per second against load, for a finished run that visited more than
  # one level.
  #
  # The live page charts the same measure against time; this charts it against
  # people, because for a completed run the question is "how much work could it
  # get through, and did adding people stop helping?"
  #
  # Only a stepped ramp can answer that. A run holding one population has one
  # point and no curve — see #throughput_over_time_chart, and Run#load_curve?
  # for which of the two a run gets.
  def throughput_by_load_chart(run)
    levels = run.level_stats.to_a.sort_by(&:vus).select(&:requests_per_second)
    return content_tag(:p, "Not enough data to chart.", class: "muted") if levels.empty?

    points = levels.map { |l| [ l.vus, l.requests_per_second ] }
    plotted_chart(
      points,
      y_label: "requests completed per second",
      x_label: "people using it at once"
    )
  end

  # Requests per second over the life of a run.
  #
  # This is what a single-level run has to show, and it is the honest chart for
  # one: a run that holds one population has no load curve, but it does have a
  # shape over time — the join burst, the plateau, and whether the plateau held
  # or sagged as the server fell behind.
  #
  # Returns nil rather than a placeholder when a run recorded nothing at all, so
  # the caller can say why instead of drawing an empty frame.
  def throughput_over_time_chart(run)
    samples = run.throughput_samples.to_a
    return nil if samples.empty?

    origin = samples.first.at.to_i
    points = samples.map { |sample| [ sample.at.to_i - origin, sample.requests.to_f ] }

    time_series_chart(
      points,
      y_label: "requests completed per second",
      x_label: "seconds since the test started"
    )
  end

  # The main chart: how slow a page got as more people used it.
  #
  # Plotted against people rather than against time, because the question is
  # "how does it behave under load", not "what happened at 14:32". Levels are
  # spaced evenly rather than by their numeric value: they are usually chosen as
  # a doubling series, and linear spacing would crush the interesting early ones
  # against the axis.
  def response_by_load_chart(run)
    levels = run.level_stats.to_a.sort_by(&:vus).select(&:room_open_p95)
    return content_tag(:p, "Not enough data to chart.", class: "muted") if levels.empty?

    points = levels.map { |l| [ l.vus, l.room_open_p95 / 1000.0 ] }
    plotted_chart(
      points,
      y_label: "seconds to open a room",
      x_label: "people using it at once",
      reference: { value: 1.0, label: "1 second" }
    )
  end

  # How hard the app worked, as a share of what it is allowed to use.
  #
  # Shown as a percentage of the app's own ceiling rather than Docker's raw
  # percent-of-one-core, which is unreadable without knowing there are six
  # workers and what a "core percent" is.
  def effort_by_load_chart(run)
    levels = run.level_stats.to_a.sort_by(&:vus).select(&:cpu_avg_pct)
    return nil if levels.empty?

    points = levels.map { |l| [ l.vus, l.cpu_avg_pct / Analysis.cpu_ceiling_pct * 100 ] }
    plotted_chart(
      points,
      y_label: "% of the server it can use",
      x_label: "people using it at once",
      reference: { value: 100.0, label: "flat out" },
      y_max: 100.0
    )
  end

  # A small labelled line chart with real axes.
  #
  # Distinct from line_chart, which is a bare sparkline for a dense time series.
  # Here there are a handful of points and each one matters, so they get drawn,
  # and both axes get ticks a reader can actually read values off.
  def plotted_chart(points, y_label:, x_label:, reference: nil, y_max: nil)
    ceiling = ceiling_for(points.map(&:last), y_max: y_max, reference: reference)
    y_for = y_scale(ceiling)

    x_for = ->(index) do
      return PLOT_LEFT + plot_width / 2.0 if points.length == 1
      PLOT_LEFT + (index.to_f / (points.length - 1)) * plot_width
    end

    coords = points.each_with_index.map { |(_, value), i| [ x_for.call(i).round(2), y_for.call(value).round(2) ] }

    plot(y_label: y_label, x_label: x_label) do
      parts = value_guides(ceiling, y_for)
      parts.concat(reference_line(reference, ceiling, y_for))

      if coords.length > 1
        parts << tag.polyline(points: coords.map { |x, y| "#{x},#{y}" }.join(" "), class: "chart-line")
      end

      # Each level is a measurement in its own right, so it gets a mark and its
      # own tick. That only holds while there are a handful of them -- a dense
      # series uses #time_series_chart instead.
      coords.each_with_index do |(x, y), i|
        parts << tag.circle(cx: x, cy: y, r: 3.5, class: "chart-point")
        parts << tag.text(points[i].first.to_s, x: x, y: PLOT_HEIGHT - PLOT_BOTTOM + 18, class: "chart-tick chart-tick--x")
      end

      parts
    end
  end

  # A dense series against a continuous x axis, in the same frame as
  # plotted_chart.
  #
  # The two differ in what an x value means, which is what decides how it is
  # drawn. plotted_chart's x values are levels -- a handful of measurements,
  # evenly spaced, each worth a mark and a tick of its own. Here x is elapsed
  # time and there are hundreds of points, so they are spaced by their real
  # value, drawn as a line with no marks, and ticked at round intervals.
  def time_series_chart(points, y_label:, x_label:)
    return content_tag(:p, "Not enough data to chart.", class: "muted") if points.blank?

    ceiling = ceiling_for(points.map(&:last))
    y_for = y_scale(ceiling)

    # Always from zero, however late the first sample landed: the x axis is
    # elapsed time, and starting it at the first point would silently crop the
    # opening of the run.
    span = [ points.map(&:first).max.to_f, 1.0 ].max
    x_for = ->(seconds) { PLOT_LEFT + (seconds.to_f / span) * plot_width }

    plot(y_label: y_label, x_label: x_label) do
      parts = value_guides(ceiling, y_for)

      coords = points.map { |seconds, value| "#{x_for.call(seconds).round(2)},#{y_for.call(value).round(2)}" }
      parts << tag.polyline(points: coords.join(" "), class: "chart-line")

      0.step(span.floor, tick_interval_seconds(span)) do |seconds|
        parts << tag.text(
          "#{seconds}s",
          x: x_for.call(seconds).round(2),
          y: PLOT_HEIGHT - PLOT_BOTTOM + 18,
          class: "chart-tick chart-tick--x"
        )
      end

      parts
    end
  end

  # CPU over the life of a run, with the host's full capacity marked.
  def cpu_chart(samples)
    return content_tag(:p, "No server samples recorded.", class: "muted") if samples.blank?

    origin = samples.first.at.to_i
    points = samples.filter_map do |sample|
      next if sample.cpu_pct.blank?

      [ sample.at.to_i - origin, sample.cpu_pct ]
    end

    saturation = Analysis::HOST_CORES * 100.0

    line_chart(
      points,
      threshold: saturation,
      y_max: saturation,
      label: "container cpu — dashed line is all #{Analysis::HOST_CORES} cores"
    )
  end

  # SQLite write-ahead log over the run. Growth that does not settle means
  # writes are outrunning checkpointing.
  def wal_chart(samples)
    return content_tag(:p, "No server samples recorded.", class: "muted") if samples.blank?

    origin = samples.first.at.to_i
    points = samples.filter_map do |sample|
      next if sample.wal_bytes.blank?

      [ sample.at.to_i - origin, sample.wal_bytes / 1_048_576.0 ]
    end

    line_chart(points, label: "sqlite wal — megabytes")
  end

  private

  # The frame both axed charts are drawn in: the svg element, its guides and its
  # axis labels. Only the data differs between them, so only the data is passed
  # in -- the block returns the parts that go inside.
  def plot(y_label:, x_label:)
    tag.svg(
      class: "chart chart--plotted",
      viewBox: "0 0 #{PLOT_WIDTH} #{PLOT_HEIGHT}",
      role: "img",
      "aria-label": "#{y_label} against #{x_label}"
    ) do
      parts = yield

      parts << tag.text(x_label, x: PLOT_LEFT + plot_width / 2.0, y: PLOT_HEIGHT - 6,
        class: "chart-axis-label chart-axis-label--x")
      parts << tag.text(y_label, x: 0, y: 0, class: "chart-axis-label",
        transform: "translate(12, #{PLOT_TOP + plot_height / 2.0}) rotate(-90)")

      safe_join(parts)
    end
  end

  # Horizontal guides, faint: enough to read a value against, not enough to
  # compete with the data (the style guide's hairline rule).
  def value_guides(ceiling, y_for)
    4.downto(0).map do |step|
      value = ceiling * step / 4.0
      y = y_for.call(value).round(2)
      [
        tag.line(x1: PLOT_LEFT, y1: y, x2: PLOT_WIDTH - PLOT_RIGHT, y2: y, class: "chart-guide"),
        tag.text(format_tick(value), x: PLOT_LEFT - 8, y: y + 3, class: "chart-tick chart-tick--y")
      ]
    end.flatten
  end

  # A dashed reference line, skipped when it would fall outside the plotted
  # range -- a line pinned to the top of every chart teaches the reader nothing.
  def reference_line(reference, ceiling, y_for)
    return [] if reference.nil? || reference[:value] > ceiling

    y = y_for.call(reference[:value]).round(2)
    [
      tag.line(x1: PLOT_LEFT, y1: y, x2: PLOT_WIDTH - PLOT_RIGHT, y2: y, class: "chart-threshold"),
      tag.text(reference[:label], x: PLOT_WIDTH - PLOT_RIGHT, y: y - 5, class: "chart-tick chart-tick--reference")
    ]
  end

  # Anchored at zero, always: a chart of a rate whose baseline is hidden
  # exaggerates variation.
  def ceiling_for(values, y_max: nil, reference: nil)
    ceiling = [ y_max, values.max, reference&.fetch(:value, nil) ].compact.max.to_f
    return 1.0 if ceiling <= 0

    # A little headroom so the peak is not welded to the top edge. Not applied to
    # an explicit maximum, which is a meaningful ceiling rather than a fitted one.
    y_max ? ceiling : ceiling * 1.1
  end

  def y_scale(ceiling)
    ->(value) { PLOT_TOP + plot_height - (value.to_f / ceiling * plot_height) }
  end

  def plot_width  = PLOT_WIDTH - PLOT_LEFT - PLOT_RIGHT
  def plot_height = PLOT_HEIGHT - PLOT_TOP - PLOT_BOTTOM

  # Round intervals a person would count in, so a minute-long run is ticked every
  # ten seconds and an hour-long one is not ticked 360 times.
  TICK_INTERVALS_SECONDS = [ 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600 ].freeze
  MAX_TIME_TICKS = 8

  def tick_interval_seconds(span)
    TICK_INTERVALS_SECONDS.find { |interval| span / interval <= MAX_TIME_TICKS } ||
      TICK_INTERVALS_SECONDS.last
  end

  def format_axis_value(value)
    value >= 100 ? value.round.to_s : format("%.1f", value)
  end

  # Axis ticks round to something a person would say out loud.
  def format_tick(value)
    if value >= 10
      value.round.to_s
    elsif value >= 1
      format("%.1f", value)
    elsif value.zero?
      "0"
    else
      format("%.2f", value)
    end
  end
end
