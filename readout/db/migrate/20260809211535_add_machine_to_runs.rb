# Which machine generated a run's load.
#
# Load used to come from exactly one box, so the question never arose. It can now
# come from either the Mac or the wired Fedora desktop, and the answer changes
# how the numbers read: the desktop shares a LAN with the Campfire host, while
# the Mac is on Wi-Fi where a ~400KB room page can saturate the link before the
# application saturates.
#
# Null for every run recorded before bin/run.sh wrote this, and left null rather
# than backfilled with a guess. Those runs all came from the Mac, but a column
# that says so on the strength of an assumption is indistinguishable from one
# that knows, and this dashboard exists to tell measurements from assumptions.
class AddMachineToRuns < ActiveRecord::Migration[8.1]
  def change
    add_column :runs, :machine, :string
  end
end
