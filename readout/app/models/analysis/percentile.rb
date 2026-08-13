module Analysis
  # Linear-interpolated percentile, matching the definition k6 itself reports so
  # the dashboard's p95 and k6's p95 mean the same thing.
  module Percentile
    def self.of(values, percent)
      return nil if values.empty?

      ordered = values.sort
      return ordered.first if ordered.length == 1

      rank = (ordered.length - 1) * percent / 100.0
      lower = rank.floor
      upper = [ lower + 1, ordered.length - 1 ].min

      ordered[lower] + (ordered[upper] - ordered[lower]) * (rank - lower)
    end
  end
end
