module Harness
  # One machine that can generate load, as this dashboard addresses it.
  #
  # `key` is the identity everywhere: the value the machine picker posts, the
  # name the runner is started with (`bin/runner --machine`), and what bin/run.sh
  # writes into each run's run-config.txt. `name` is only what a person reads.
  Runner = Data.define(:key, :name, :url, :token_file) do
    def client = Harness::Client.new(url: url, token_file: token_file)

    # Whether the machine that answered is the one this entry addresses.
    #
    # The dashboard reaches runners by IP — a container on the VPS cannot
    # resolve MagicDNS, so addresses are resolved on the host and passed in —
    # and a tailnet IP is not a constant. If one is reassigned, the wrong box
    # answers perfectly happily.
    #
    # A runner that reports no machine at all is accepted rather than doubted:
    # `--machine` is optional, and refusing an unnamed runner would be inventing
    # a fault out of a runner that is merely older than this feature.
    def answers_as?(reported) = reported.blank? || reported == key
  end
end
