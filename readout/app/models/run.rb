class Run < ApplicationRecord
  # The knee is stated as a ratio against the run's own healthiest level rather
  # than an absolute millisecond threshold: what counts as "slow" depends
  # entirely on what this hardware does when it is not under pressure.
  DEGRADATION_FACTOR = 3.0

  has_many :level_stats, -> { order(:vus) }, dependent: :delete_all
  has_many :server_samples, -> { order(:at) }, dependent: :delete_all
  has_many :throughput_samples, -> { order(:at) }, dependent: :delete_all

  # The live charts' series, kept out of every list query: see RunProgress.
  has_one :progress, class_name: "RunProgress", dependent: :delete

  validates :stamp, presence: true, uniqueness: true

  # Runs are addressed by stamp everywhere: it is what the runner calls this
  # run, what its results directory is named, and what survives the database
  # being replaced wholesale by bin/publish. A database id means nothing outside
  # the instance that assigned it, and a run in flight does not have one yet.
  def to_param = stamp

  scope :newest_first, -> { order(started_at: :desc, stamp: :desc) }

  def settings
    @settings ||= config.present? ? JSON.parse(config) : {}
  rescue JSON::ParserError
    {}
  end

  def peak_vus
    level_stats.maximum(:vus)
  end

  # Whether this run can say which server it measured.
  #
  # Three states, and collapsing them would be a lie in both directions: a name
  # means the harness read it off the box, "unknown" means it looked and could
  # not tell, and nil means the run predates recording it at all. Only the first
  # can carry a comparison.
  UNKNOWN_VARIANT = "unknown".freeze

  def variant_known?
    variant.present? && variant != UNKNOWN_VARIANT
  end

  def variant_label
    return variant if variant_known?
    return "server unidentified" if variant == UNKNOWN_VARIANT

    "server not recorded"
  end

  # Which machine generated this run's load, as a person reads it.
  #
  # The stored value is the key the machine calls itself (`bin/runner
  # --machine`), so it is resolved through the configured fleet to get the
  # display name — and falls back to the key when a machine has since been
  # renamed or removed, rather than dropping the fact that it ran somewhere.
  #
  # Nil means the run predates recording this, which is left as its own state
  # rather than backfilled: every such run did come from the Mac, but a label
  # that says so on the strength of an assumption is indistinguishable from one
  # that knows.
  def machine_known? = machine.present?

  def machine_label
    return nil unless machine_known?

    Harness::Fleet.current.find(machine)&.name || machine
  end

  # Whether this run visited more than one load level, i.e. whether there is a
  # curve to plot throughput against.
  #
  # Most runs no longer do. A stepped ramp answers "where does it break" and
  # holds several levels; chat and enterprise — the two the dashboard launches —
  # answer "what does this size cost" and deliberately hold exactly one. Charting
  # a single-level run against load draws one dot, so those are charted against
  # time instead.
  def load_curve?
    level_stats.to_a.count { |stat| stat.requests_per_second.present? } >= 2
  end

  # Share of the run's requests the app never answered.
  #
  # The proportion is the number that means something: 157 unanswered is a bad
  # run at 862 requests and a rounding error at 300,000.
  def unanswered_share
    return nil if total_requests.blank? || total_requests.zero? || unanswered_requests.blank?

    unanswered_requests.to_f / total_requests
  end

  def answered_requests
    return nil if total_requests.blank?

    total_requests - unanswered_requests.to_i
  end

  def peak_cpu_share_of_host
    return nil if peak_cpu_pct.blank?

    peak_cpu_pct / Analysis::HOST_CORES
  end

  # Share of what the app can actually use, which is what "was it working flat
  # out?" really means. See Analysis::TARGET_WORKERS.
  def peak_cpu_share_of_capacity
    return nil if peak_cpu_pct.blank?

    peak_cpu_pct / Analysis.cpu_ceiling_pct * 100
  end

  # The busiest level that still behaved, i.e. the last one before things broke.
  def last_healthy_level
    ordered = level_stats.to_a.sort_by(&:vus)
    point = breaking_point
    return ordered.last if point.nil?

    ordered.select { |s| s.vus < point.vus }.last
  end

  # The first level whose room-open p95 is DEGRADATION_FACTOR times the run's
  # best, or nil if nothing degraded.
  def knee
    @knee ||= begin
      stats = level_stats.to_a.select(&:room_open_p95)
      baseline = stats.map(&:room_open_p95).min

      if stats.length < 2 || baseline.nil? || baseline.zero?
        nil
      else
        stats.sort_by(&:vus).find { |s| s.room_open_p95 >= baseline * DEGRADATION_FACTOR }
      end
    end
  end

  def cpu_saturated?
    peak_cpu_pct.present? && peak_cpu_pct >= Analysis.cpu_saturation_threshold_pct
  end

  # The first level that was slow in absolute terms, regardless of how the rest
  # of the run compared. Catches a run whose every level was bad.
  def first_struggling_level
    level_stats.to_a.sort_by(&:vus).find do |s|
      s.room_open_p95.present? && s.room_open_p95 >= Analysis::SLOW_PAGE_MS
    end
  end

  # Where this run stopped being acceptable — by either measure, whichever came
  # first. This, not #knee, is what a verdict should be built on.
  def breaking_point
    [ knee, first_struggling_level ].compact.min_by(&:vus)
  end

  def healthy?
    breaking_point.nil?
  end

  # True when the run degraded while the server still had CPU headroom, meaning
  # the limit was not application compute. On a wireless host the usual
  # explanation is the link, not the app -- worth saying out loud rather than
  # letting the latency table imply an application verdict it cannot support.
  def degraded_without_cpu_saturation?
    breaking_point.present? && peak_cpu_pct.present? && !cpu_saturated?
  end

  # Fraction of headroom below which added load is judged not to have bought
  # any more throughput.
  PLATEAU_TOLERANCE = 0.1

  # Throughput that stops rising while load keeps climbing is the signature of a
  # hard ceiling somewhere below the application.
  #
  # Compares the top two levels specifically. Comparing the last level against
  # the run's maximum instead would call every healthy run a plateau, since in a
  # run that scales properly the last level *is* the maximum.
  def throughput_plateaued?
    rates = level_stats.to_a.sort_by(&:vus).filter_map(&:megabytes_per_second)
    return false if rates.length < 2

    previous, last = rates[-2], rates[-1]
    return false if previous.nil? || previous.zero?

    last <= previous * (1 + PLATEAU_TOLERANCE)
  end
end
