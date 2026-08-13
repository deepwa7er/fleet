# Shared by the controllers that drive a runner service.
#
# Launching a load test and switching which server it measures are both only
# meaningful where a runner is actually reachable — a dashboard with no harness
# beside it has neither k6 nor the machine under test — so both are gated the
# same way rather than each deciding for itself.
module RunnerAccess
  extend ActiveSupport::Concern

  included do
    before_action :require_runner
  end

  private

  def fleet = @fleet ||= Harness::Fleet.current

  # The machines a run may be launched on right now.
  def available_machines = fleet.available

  # A runner to ask about the machine under test.
  #
  # Any of them will do, and that is not laziness: what is deployed to Campfire
  # is a property of the Campfire host, not of the generator. Every runner reads
  # it off that same box (`bin/campfire-variant`), so they all give the same
  # answer and the first one to answer is as good as any.
  def client = available_machines.first&.runner&.client

  def require_runner
    return if available_machines.any?

    redirect_to runs_path, alert: no_runner_message
  end

  # Said differently depending on why, because the two have different fixes.
  #
  # A machine answering under another machine's name is a resolved address
  # pointing at the wrong box, and telling someone to start a runner they are
  # already running would send them the wrong way entirely.
  def no_runner_message
    mismatched = fleet.unavailable.filter_map(&:mismatch)
    return mismatched.to_sentence if mismatched.any?

    "No machine is reachable. Load tests run on the machine with the harness on " \
      "it — start it there with `bin/runner` in campfire-stress."
  end
end
