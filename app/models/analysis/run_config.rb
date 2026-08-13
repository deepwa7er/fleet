# Reads the run-config.txt that bin/run.sh writes beside each set of results.
#
# A results directory that cannot be tied back to the configuration that
# produced it is not evidence of anything, so this is treated as part of the
# data rather than as decoration.
module Analysis
  class RunConfig
    # What the run was, as opposed to what was asked of it. These describe the
    # harness and the server rather than the load, so they are excluded from the
    # settings a page lists.
    META_KEYS = %w[
      stamp scenario target generator machine k6
      variant server_image server_digest server_env
    ].freeze

    KNOWN_KEYS = (META_KEYS + %w[
      VUS USER_POOL SESSION_SECONDS RAMP_UP HOLD RAMP_DOWN
      HOT_ROOM_SHARE HOT_ROOM_ID THINK_MIN_MS THINK_MAX_MS
      W_READ_HISTORY W_SWITCH_ROOM W_POST_MESSAGE W_IDLE
    ]).freeze

    # bin/run.sh writes this placeholder when a variable was left unset.
    UNSET = "<config default>"

    attr_reader :values

    def initialize(values)
      @values = values
    end

    def self.parse(path)
      return new({}) unless path && File.exist?(path)

      values = {}
      File.foreach(path) do |line|
        key, value = line.strip.split("=", 2)
        next if key.nil? || value.nil? || value.empty?

        values[key] = value
      end

      new(values)
    end

    def [](key) = @values[key]

    def scenario = @values["scenario"]
    def target = @values["target"]
    def generator = @values["generator"]
    def k6_version = @values["k6"]

    # Which generator machine produced this run, as the dashboard names it.
    #
    # Distinct from #generator, which is the box's own hostname. The two Fedora
    # boxes on this tailnet both answer `fedora` to `hostname`, so the hostname
    # cannot be relied on to tell one generator from another — and the machine
    # matters to the numbers, since one generator is wired to the target's LAN
    # and the other is on Wi-Fi.
    #
    # Absent for runs from before bin/run.sh recorded it. Those say so rather
    # than being attributed to whichever machine seems likely.
    def machine = @values["machine"]

    # The server this run measured. Absent for runs that predate bin/run.sh
    # recording it, and "unknown" when the harness could not read it off the
    # box — both of which must stay distinguishable from a name, since a run
    # that cannot say what it measured is not evidence about any variant.
    def variant = @values["variant"]
    def server_image = @values["server_image"]
    def server_digest = @values["server_digest"]
    def server_env = @values["server_env"]

    # Only the settings that were explicitly set for this run; the rest came
    # from config.js defaults and would be noise on the page.
    def explicit_settings
      @values
        .slice(*KNOWN_KEYS)
        .reject { |_, value| value == UNSET }
        .except(*META_KEYS)
    end
  end
end
