require "json"

module Harness
  # The machines that can generate load, and what each is doing right now.
  #
  # There used to be exactly one, so "the runner" was a singular thing the
  # dashboard could assume. Load can now come from either the Mac or the wired
  # Fedora desktop, and which one produced a set of numbers changes how they
  # read — the desktop shares a LAN with the Campfire host, while the Mac is on
  # Wi-Fi where a ~400KB room page can saturate the link before the application
  # does.
  #
  # Everything here fails soft. A runner is optional infrastructure: the deployed
  # instance has none of its own, a machine may be asleep, and the dashboard's
  # read-only half has to keep working through all of it.
  class Fleet
    # Machines are described by one JSON value rather than a family of
    # environment variables, because the VPS resolves them at service start —
    # see deploy/provision.sh — and writes the result in one go.
    ENV_KEY = "RUNNERS".freeze

    attr_reader :runners

    def initialize(runners)
      @runners = runners
    end

    def self.current = new(configured)

    def self.configured
      raw = Rails.configuration.x.runners
      return default if raw.blank?

      parse(raw)
    end

    # A parse failure must not take the dashboard down: results are readable
    # without any runner at all, and a typo in one environment variable is not a
    # reason to lose the whole app. It is loud in the log and silent on the page.
    def self.parse(raw)
      entries = raw.is_a?(String) ? JSON.parse(raw) : raw
      raise TypeError, "expected a list of runners" unless entries.is_a?(Array)

      entries.filter_map { |entry| build(entry) }
    rescue JSON::ParserError, TypeError => e
      Rails.logger.error("#{ENV_KEY} could not be read (#{e.message}); no machines will be offered")
      []
    end

    def self.build(entry)
      return nil unless entry.is_a?(Hash)

      key = entry["key"].presence
      url = entry["url"].presence

      # An entry the dashboard could neither address nor name is dropped rather
      # than half-built. The resolver leaves `url` empty for a machine it could
      # not find, which is the ordinary way a sleeping laptop arrives here.
      return nil if key.nil? || url.nil?

      Runner.new(
        key: key,
        name: entry["name"].presence || key,
        url: url,
        token_file: entry["token_file"].presence
      )
    end
    private_class_method :build

    # What a machine with no configuration at all should assume: the runner on
    # this box, reached over loopback. This is the developer's case — readout and
    # the harness on the same Mac — and it keeps `bin/readout` working with
    # nothing set.
    def self.default
      [ Runner.new(
        key: "mac",
        name: "MacBook",
        url: "http://127.0.0.1:7881",
        token_file: File.expand_path("~/code/campfire-stress/.runner-token")
      ) ]
    end

    def find(key) = runners.find { |runner| runner.key == key }

    def any? = runners.any?

    # Every machine that answered, paired with what it said.
    #
    # One call per machine, not two: /healthz reports both that a runner is there
    # and whether it is busy, and this sits in the critical path of pages that
    # render on every visit.
    def health
      @health ||= runners.filter_map do |runner|
        reported = runner.client.health
        next if reported.blank?

        Presence.new(runner: runner, health: reported)
      end
    end

    # The machines a run may actually be launched on.
    #
    # A machine that answers under another machine's name is left out. That is a
    # resolved address pointing at the wrong box, and launching there would
    # produce a run recorded as one machine while the picker said another —
    # bin/run.sh records what actually ran it, so the results stay honest and
    # only the request was a lie. Better to refuse than to create it.
    def available = health.select(&:answers_as_itself?)

    def unavailable = health.reject(&:answers_as_itself?)

    # The run holding the fleet, if any.
    #
    # Only one may be under way anywhere. This is a correctness rule rather than
    # a resource one: every generator points at the same Campfire instance, so
    # two runs on two machines would each measure the other's load. The runner
    # enforces one-at-a-time within its own process and cannot see the others —
    # this is the only place that can.
    def busy = health.find(&:busy?)

    # Which machine is running a given run, for a page that has only its stamp.
    #
    # Asked of every machine rather than remembered, so it also answers for runs
    # started from the command line rather than from this dashboard.
    def holding(stamp)
      health.each do |presence|
        run = presence.runner.client.run(stamp)
        return [ presence.runner, run ] if run.present?
      rescue Client::Error
        next
      end

      nil
    end

    # A machine and what it reported, so callers do not each re-probe it.
    Presence = Data.define(:runner, :health) do
      def busy? = health["busy"].present?
      def active_run = health["active_run"]
      def reported_machine = health["machine"]
      def answers_as_itself? = runner.answers_as?(reported_machine)

      def mismatch
        return nil if answers_as_itself?

        "#{runner.name} (#{runner.url}) answers as #{reported_machine.inspect}, " \
          "not #{runner.key.inspect} — its address is pointing at another machine"
      end
    end
  end
end
