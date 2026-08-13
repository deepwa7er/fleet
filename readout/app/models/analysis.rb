# Analysis of campfire-stress result directories.
#
# A k6 run leaves behind a directory of raw CSV; this turns it into the numbers
# a person actually wants. The work is deliberately split from the Rails models:
# the parsing is plain Ruby over IO, and only Analysis::Importer knows about
# ActiveRecord.
#
# The interesting judgement lives in LevelBreakdown -- k6's own summary averages
# every ramp step together, which is exactly where a load test's knee hides.
module Analysis
  # Latency metrics reported per VU level, in display order.
  LATENCY_METRICS = {
    "campfire_room_open"        => :room_open_p95,
    "campfire_history"          => :history_p95,
    "campfire_post_message"     => :post_message_p95,
    "campfire_fanout_latency"   => :fanout_p95,
    "campfire_cable_subscribe"  => :cable_subscribe_p95
  }.freeze

  # Cumulative metrics summed over a window rather than percentiled.
  COUNTER_METRICS = %w[
    campfire_errors
    http_reqs
    campfire_broadcasts_received
    data_received
  ].freeze

  # HTTP statuses that mean the application never answered.
  #
  # These are not "errors" in the sense a 4xx is. A 404 or a 500 is the app
  # responding — it received the request, ran, and decided something. Every
  # status here means it did not:
  #
  #   0    no response reached the client at all: the connection failed, was
  #        reset, or timed out client-side. k6 records the request with no status.
  #   502  kamal-proxy could not get a usable response out of the app.
  #   503  kamal-proxy had no healthy target to send it to.
  #   504  the app did not answer before the proxy gave up waiting.
  #
  # The 5xx three are answers the *proxy* invented because the app produced
  # none, which is why they belong here rather than with application errors.
  UNANSWERED_STATUSES = [ 0, 502, 503, 504 ].to_set.freeze

  # Fraction of each level's window discarded as settling time. A level that has
  # just been stepped into is still filling connection pools and warming caches,
  # and including that transient makes every step look worse than it is.
  SETTLE_FRACTION = 0.25

  # A level must be held at least this long to count as a plateau rather than a
  # value the ramp merely passed through on its way somewhere else.
  MIN_HOLD_SECONDS = 30

  # fedora-1 has 8 cores, and Docker reports CPU as percent of ONE core -- so
  # 800% would be full saturation of the *machine*.
  HOST_CORES = 8

  # But the machine's core count is the wrong ceiling to judge Campfire against.
  #
  # Puma runs 6 worker processes (`(8 * 0.666).ceil` in its config), and Ruby's
  # global lock means each worker executes Ruby on at most one core. The app can
  # therefore never exceed ~600% however busy it gets, so a run peaking at 558%
  # is not "70% of the box with headroom to spare" -- it is 93% of everything
  # this app is permitted to use.
  #
  # Measuring against HOST_CORES produced exactly that misreading: the dashboard
  # called a saturated app "not compute bound" and pointed at the network, which
  # was wrong. Judge against the workers.
  TARGET_WORKERS = 6
  TARGET_THREADS_PER_WORKER = 5

  # Requests the server can work on at once. Past this, work queues -- and a
  # queue is why latency collapses suddenly instead of degrading gently.
  REQUEST_SLOTS = TARGET_WORKERS * TARGET_THREADS_PER_WORKER

  # Above this, a page load is bad on its own terms, whatever the rest of the run
  # looks like.
  #
  # A purely relative test ("3x the run's own best") cannot judge a run with only
  # one level, because there is nothing to compare against — so a single level
  # sitting at nine seconds was being reported as "handled everything, no sign of
  # strain". An absolute floor is what stops that.
  #
  # One second is a judgement, not a measurement: for a chat app that renders a
  # room on every click, a second is where it stops feeling immediate.
  SLOW_PAGE_MS = 1000

  CPU_SATURATION_FRACTION = 0.85

  # Percent-of-one-core the app can reach with every worker busy.
  def self.cpu_ceiling_pct
    TARGET_WORKERS * 100
  end

  def self.cpu_saturation_threshold_pct
    cpu_ceiling_pct * CPU_SATURATION_FRACTION
  end
end
