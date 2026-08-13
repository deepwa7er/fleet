# Turns a run's numbers into plain language.
#
# The table below it answers "what happened". This answers "so what" — which is
# the question you actually have after a load test, and the one a grid of p95
# figures does not answer on its own.
#
# Rules it tries to keep:
#   * No jargon. Seconds, not milliseconds; "people at once", not VUs.
#   * Say what the evidence supports and nothing further. Where the data cannot
#     distinguish two explanations, say that rather than picking the tidier one.
#   * Never blame the application without checking it was actually working flat
#     out first — measured against what the app can use, not the machine's size.
class RunNarrative
  def initialize(run)
    @run = run
    @levels = run.level_stats.to_a.sort_by(&:vus)
  end

  def present? = @levels.any?

  # One sentence, the thing to read if you read nothing else.
  def headline
    return "This run did not get far enough to measure anything." if @levels.empty?

    point = @run.breaking_point

    if point.nil?
      "Handled everything it was given — up to #{people(@levels.last.vus)} at once, " \
      "with no sign of strain."
    else
      healthy = @run.last_healthy_level
      if healthy
        "Comfortable up to #{people(healthy.vus)} at once. Falls over at #{people(point.vus)}."
      else
        # Every level was already bad, so there is no "comfortable up to" to report.
        "Struggling from the start — even at #{people(point.vus)}, pages took " \
        "#{seconds(point.room_open_p95)}."
      end
    end
  end

  # [{ title:, body: }] — the findings, in the order worth reading them.
  def findings
    return [] if @levels.empty?

    # When no level behaved, there is no baseline, and "reading is cheap" beside
    # a nine-second page load is a contradiction. Report the collapse instead.
    return [ collapse_finding, headroom_finding ].compact if @run.last_healthy_level.nil?

    [ reading_finding, writing_finding, collapse_finding, headroom_finding ].compact
  end

  def suggestions
    return [] if @levels.empty?

    list = []

    if @run.breaking_point && @run.cpu_saturated?
      list << "Let Campfire use more of the machine. It runs #{Analysis::TARGET_WORKERS} " \
              "workers on an #{Analysis::HOST_CORES}-core box, so there is room for more. " \
              "That is two settings and a restart — re-run this same test afterwards and " \
              "see whether the cliff moves."
    end

    if slowest_write && slowest_write > fastest_read * 2
      list << "Make sending a message cheaper. It costs about " \
              "#{seconds(slowest_write)} against #{seconds(fastest_read)} for reading a " \
              "page, and every slow write holds a slot that something else is waiting for."
    end

    if @run.healthy? && @levels.last
      list << "Push harder. Nothing here was struggling, so the real limit is above " \
              "#{people(@levels.last.vus)} — run it again with a bigger number."
    end

    list
  end

  # Stated whenever it applies, because it changes how the numbers should be
  # read and is invisible from the table.
  def caveat
    return nil unless @run.scenario.to_s.match?(/browsing|ramp/)

    "This test slows down when the server does: each simulated person waits for " \
    "their page before asking for the next one, exactly like a real user would. " \
    "So the load actually offered drops just as the server starts struggling, and " \
    "the real cliff may be sharper than it looks here. The “arrival” scenario " \
    "keeps pushing regardless."
  end

  # How to read the response-time chart: what a value on it actually feels like.
  #
  # Stated as bands rather than one threshold because "is 1.4 seconds bad?" has
  # no yes/no answer, and a single pass/fail line would invent one.
  RESPONSE_BANDS = [
    { limit: 0.5, name: "Under 0.5s", meaning: "feels instant — nobody notices the wait." },
    { limit: 2.0, name: "0.5 to 2s", meaning: "noticeable, but the app still feels alive." },
    { limit: 5.0, name: "2 to 5s", meaning: "slow enough to be annoying; people start clicking twice." },
    { limit: Float::INFINITY, name: "Over 5s", meaning: "people assume it is broken and give up." }
  ].freeze

  EFFORT_BANDS = [
    { limit: 50, name: "Under 50%", meaning: "coasting — plenty left in reserve." },
    { limit: 85, name: "50 to 85%", meaning: "working hard, but still keeping up." },
    { limit: Float::INFINITY, name: "Over 85%", meaning: "flat out — this is the app's ceiling, not the machine's." }
  ].freeze

  # The band a value falls into, so the page can point at the row that applies.
  def self.band_for(value, bands)
    bands.find { |band| value < band[:limit] } || bands.last
  end

  # Worst page load in the run, in seconds — what the response chart peaks at.
  def worst_response_seconds
    value = @levels.filter_map(&:room_open_p95).max
    value && value / 1000.0
  end

  # Hardest the app worked, as a share of its own ceiling.
  def peak_effort_percent
    @run.peak_cpu_share_of_capacity
  end

  private

  attr_reader :run, :levels

  def reading_finding
    reads = @levels.filter_map(&:room_open_p95)
    return nil if reads.empty?

    healthy = healthy_levels.filter_map(&:room_open_p95)
    body =
      if healthy.length > 1
        "Opening a room took about #{seconds(healthy.min)} whether " \
        "#{people(healthy_levels.first.vus)} or #{people(healthy_levels.last.vus)} " \
        "were using it. Reading is not what breaks."
      else
        "Opening a room took about #{seconds(reads.min)}."
      end

    { title: "Reading pages is cheap", body: body }
  end

  def writing_finding
    return nil if slowest_write.nil? || fastest_read.nil?

    quietest = healthy_levels.first
    body = "Sending a message took about #{seconds(slowest_write)} — around " \
           "#{(slowest_write / fastest_read).round} times the cost of reading a page."

    if quietest && quietest.post_message_p95 && quietest.cpu_avg_pct.to_f < 50
      body += " It cost roughly that much even at #{people(quietest.vus)}, when the " \
              "server was nearly idle, so this is what one message costs rather than " \
              "the effect of crowding."
    end

    { title: "Sending a message is the expensive part", body: body }
  end

  def collapse_finding
    point = @run.breaking_point
    return nil if point.nil?

    healthy = @run.last_healthy_level
    slow = point.room_open_p95

    body =
      if healthy
        +"At #{people(point.vus)} the same page went from " \
         "#{seconds(healthy.room_open_p95)} to #{seconds(slow)}. "
      else
        +"Even at #{people(point.vus)}, opening a room took #{seconds(slow)}. "
      end


    body << "The server works on about #{Analysis::REQUEST_SLOTS} requests at a time. " \
            "Past a certain point requests arrive faster than it can finish them, so a " \
            "queue builds up and everyone waits — including someone who only wanted one " \
            "page. That is why it collapses suddenly instead of getting gradually worse."

    { title: "Why it falls over", body: body }
  end

  def headroom_finding
    return nil if @run.peak_cpu_pct.blank?

    share = @run.peak_cpu_share_of_capacity.round
    of_box = @run.peak_cpu_share_of_host.round

    body =
      if @run.cpu_saturated?
        "Campfire was running at about #{share}% of everything it is allowed to use. " \
        "It only gets #{Analysis::TARGET_WORKERS} of the machine's #{Analysis::HOST_CORES} " \
        "cores, so although this looks like only #{of_box}% of the computer, the app " \
        "itself was working flat out."
      else
        "Campfire peaked at about #{share}% of what it is allowed to use " \
        "(#{Analysis::TARGET_WORKERS} of the machine's #{Analysis::HOST_CORES} cores), " \
        "so it still had room to work harder. Whatever limited this run, it was not " \
        "the app running out of processor."
      end

    if @run.throughput_plateaued?
      body += " Data transferred also stopped rising between the last two levels, which " \
              "usually means something below the app hit a ceiling."
    end

    { title: "How hard the server was working", body: body }
  end

  def healthy_levels
    @healthy_levels ||= begin
      point = @run.breaking_point
      point ? @levels.select { |s| s.vus < point.vus }.presence || @levels : @levels
    end
  end

  def fastest_read
    @fastest_read ||= @levels.filter_map(&:room_open_p95).min
  end

  # The typical cost of a write when things were healthy -- the floor, not the
  # figure from a level that had already collapsed.
  def slowest_write
    @slowest_write ||= healthy_levels.filter_map(&:post_message_p95).min
  end

  def people(count)
    "#{count} #{'person'.pluralize(count)}"
  end

  # Milliseconds are not a unit anyone thinks in. Seconds, at a precision that
  # does not pretend to more accuracy than a percentile deserves.
  def seconds(ms)
    return "—" if ms.blank?

    value = ms / 1000.0
    if value < 0.1
      "a twentieth of a second"
    elsif value < 1
      "#{value.round(1)} seconds"
    elsif value < 10
      "#{value.round(1)} seconds"
    else
      "#{value.round} seconds"
    end
  end
end
