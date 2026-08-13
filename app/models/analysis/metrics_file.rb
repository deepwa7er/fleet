# Streams a k6 metrics.csv into the few series we actually need.
#
# These files reach ~700k lines / 80MB, so the whole file is never held in
# memory and CSV.parse is avoided entirely. Only the first three columns are
# read (metric_name, timestamp, metric_value); all three are simple scalars that
# cannot contain a comma, so a bounded String#split is safe here in a way that
# splitting the full row would not be -- later columns hold URLs and tag lists.
#
# The one exception is the status of an http_reqs row, which sits past several
# fields that genuinely can contain commas -- including `error`, which is only
# populated on exactly the failures being counted. Those rows are therefore
# handed to a real CSV parser rather than split. It is affordable because there
# is one such row per request, a small fraction of the file: on the largest run
# here, 28k of 695k lines.
require "csv"

module Analysis
  class MetricsFile
    Sample = Struct.new(:at, :value)

    # Column holding the HTTP status, located by name so a change to k6's column
    # order cannot silently start counting the wrong field.
    STATUS_COLUMN = "status"

    attr_reader :vu_timeline, :latency_samples, :counter_samples, :unanswered_requests

    def initialize(path)
      @path = path
      @vu_timeline = []
      @latency_samples = Hash.new { |h, k| h[k] = [] }
      @counter_samples = Hash.new { |h, k| h[k] = [] }
      @unanswered_requests = 0
      @parsed = false
    end

    def self.parse(path)
      new(path).tap(&:parse)
    end

    def parse
      return self if @parsed

      wanted_latency = LATENCY_METRICS.keys.to_set
      wanted_counter = COUNTER_METRICS.to_set

      status_index = nil

      File.open(@path, "r") do |file|
        file.each_line.with_index do |line, index|
          if index.zero?
            status_index = line.chomp.split(",").index(STATUS_COLUMN)
            next
          end

          name, timestamp, value = line.split(",", 4)
          next if value.nil?

          at = Integer(timestamp, exception: false)
          number = Float(value, exception: false)
          next if at.nil? || number.nil?

          if name == "vus"
            @vu_timeline << [ at, number.to_i ]
          elsif wanted_latency.include?(name)
            @latency_samples[name] << Sample.new(at, number)
          elsif wanted_counter.include?(name)
            @counter_samples[name] << Sample.new(at, number)
            @unanswered_requests += 1 if name == "http_reqs" && unanswered?(line, status_index)
          end
        end
      end

      # k6 writes samples in time order, but sorting makes the downstream
      # segment logic independent of that assumption.
      @vu_timeline.sort_by!(&:first)
      @parsed = true
      self
    end

    def empty?
      @vu_timeline.empty?
    end

    private

    # A request the app never answered.
    #
    # An unparseable row is not counted. Guessing either way would be worse: an
    # inflated count invents an outage, and a silently-dropped row hides one, but
    # a row k6 wrote in a shape we do not recognise is a reason to distrust this
    # figure rather than to fabricate it — and #parse would have to have been
    # given a file with no `status` column at all for that to happen.
    def unanswered?(line, status_index)
      return false if status_index.nil?

      raw = CSV.parse_line(line)&.at(status_index)

      # Integer() rather than to_i, which turns both "" and "banana" into 0 --
      # and 0 is itself one of the statuses being looked for, so a blank field
      # would report a connection failure that never happened.
      status = raw && Integer(raw, exception: false)
      return false if status.nil?

      UNANSWERED_STATUSES.include?(status)
    rescue CSV::MalformedCSVError
      false
    end
  end
end
