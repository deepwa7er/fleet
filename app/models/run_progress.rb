# The series behind a run's charts, exactly as the runner computed them.
#
# Stored rather than derived, because they cannot be derived here. They come from
# k6's JSON output — the only source of sub-second arrival times — which runs to
# 40-150MB per run and exists solely on the machine that generated the load. The
# deployed instance has neither that file nor any other raw result: it is shipped
# this database and nothing else.
#
# So the runner writes the payload it was already serving to the live chart into
# results/<stamp>/progress.json, the importer stores it verbatim, and the run's
# page hands it back to that same chart. A finished run is then drawn from the
# same numbers by the same code as a running one, rather than from a second
# implementation of the arithmetic that would have to stay in agreement with it —
# which it did not, and that divergence is the whole reason this table exists.
#
# Its own table rather than a column on runs: a payload is a few hundred KB, and
# the index page lists every run there is.
class RunProgress < ApplicationRecord
  belongs_to :run

  validates :payload, presence: true
end
