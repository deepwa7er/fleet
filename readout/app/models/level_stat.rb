class LevelStat < ApplicationRecord
  belongs_to :run

  # Display order matches the analyzer's, so the table reads the same way the
  # command-line output does.
  LATENCY_COLUMNS = [
    [ :room_open_p95,       "room open" ],
    [ :history_p95,         "history" ],
    [ :post_message_p95,    "post msg" ],
    [ :fanout_p95,          "fan-out" ],
    [ :cable_subscribe_p95, "cable sub" ]
  ].freeze

  def latencies
    LATENCY_COLUMNS.map { |column, label| [ label, public_send(column) ] }
  end

  def healthy?
    error_count.to_i.zero?
  end
end
