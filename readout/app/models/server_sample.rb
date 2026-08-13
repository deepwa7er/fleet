class ServerSample < ApplicationRecord
  belongs_to :run

  # Docker reports container CPU as a percentage of ONE core, so on the 8-core
  # host a reading of 800 means fully saturated.
  def cpu_share_of_host
    return nil if cpu_pct.blank?

    cpu_pct / Analysis::HOST_CORES
  end

  def wal_megabytes
    return nil if wal_bytes.blank?

    wal_bytes / 1_048_576.0
  end
end
