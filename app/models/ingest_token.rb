# The shared secret a generator machine uses to publish a run.
#
# Results are parsed where the load was generated and pushed here, so this
# dashboard accepts writes from machines that are not itself. That is the one
# route into this app that changes data without a person driving it, which is
# why it is gated on a secret rather than on being inside the tailnet.
#
# Read from a file rather than an environment variable, for the same reason the
# runner token and the Rails master key are: the deploy ships an image tar over
# the network and systemd units are world-readable, whereas this file is 0600 and
# mounted in.
#
# Absent is a normal state, not a fault. A local instance nobody publishes to has
# no token, and the endpoint refuses everything rather than accepting anything —
# an unconfigured dashboard that took writes from the network would be far worse
# than one that takes none.
class IngestToken
  class << self
    def configured? = value.present?

    def value
      return @value if defined?(@value)

      @value = read
    end

    # Constant-time, because a comparison that returns early leaks how much of a
    # guess was right.
    def matches?(given)
      return false unless configured?
      return false if given.blank?

      ActiveSupport::SecurityUtils.secure_compare(given, value)
    end

    # For tests and for a console that has just written the file.
    def reload!
      remove_instance_variable(:@value) if defined?(@value)
    end

    private

    def read
      path = Rails.configuration.x.ingest.token_file
      return nil if path.blank?

      File.read(path).strip.presence
    rescue SystemCallError
      nil
    end
  end
end
