# How many requests the server finished in one second of a run.
#
# Stored per run rather than derived on demand for the same reason every other
# figure here is: the metrics.csv it comes from reaches 700k lines, and this app
# is deployed away from the machine those files live on.
#
# A second in which nothing completed is stored as a zero rather than left out.
# Unlike a latency mean, which has no value when there were no samples, a rate
# genuinely is zero when nothing finished — and a gap the chart drew a straight
# line across would hide exactly the stall worth seeing.
class ThroughputSample < ApplicationRecord
  belongs_to :run
end
