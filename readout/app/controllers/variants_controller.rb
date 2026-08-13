# Which build of Campfire the next load test will measure.
#
# Switching is deliberately its own action rather than a field on the launch
# form. A switch redeploys and restarts the application under test, so it starts
# with an empty page cache: the sequence that produces comparable numbers is
# switch, discard a warm-up run, then start the run you intend to keep. A launch
# that silently restarted the server would make that impossible to get right.
class VariantsController < ApplicationController
  include RunnerAccess

  # The frame's contents. Polled while a switch is in progress, which is why it
  # is a fragment rather than a page.
  def show
    render partial: "variants/variant", locals: variant_locals(error: flash[:alert])
  end

  # Redirected rather than rendered, which is not a style choice.
  #
  # This is submitted from inside a Turbo Frame, and rendering the frame in
  # response to the POST left the frame's src pointing at this URL — which
  # accepts no GET. The next poll fetched it, got a 404, and the frame said
  # "Content missing" the moment the switch finished. Redirecting sends the
  # frame back to a URL it can keep asking for.
  #
  # The refusal survives the redirect because the frame renders the flash
  # itself. It is only lost when a redirect lands somewhere the flash is drawn
  # outside the frame, which is what made rendering look necessary.
  def switch
    client.switch_variant(params[:name])
    redirect_to variant_path, status: :see_other
  rescue Harness::Client::Busy => e
    redirect_to variant_path, status: :see_other, alert: e.message
  rescue Harness::Client::Error => e
    redirect_to variant_path, status: :see_other, alert: "Could not switch: #{e.message}"
  end

  private

  # A runner that cannot be reached mid-page must not blank the frame: the page
  # says what it could not read rather than implying there are no variants.
  #
  # KNOWN, AND DELIBERATELY NOT FIXED: this occasionally reports a timeout when
  # nothing is wrong.
  #
  # Reading the deployed variant is a live round trip on every page load —
  # readout, to the runner on the dev box, to `ssh laptop`, to `docker inspect`,
  # and back — against Harness::Client's five second budget. It normally takes
  # about 400ms. On 2026-08-08 one request took 10,159ms and the frame rendered
  # "the deployed server could not be read"; the next took 574ms. It is most
  # likely to happen mid-run, when the dev box is generating load and the
  # machine under test is under it.
  #
  # The decision is to refresh the page until the round trip lands inside five
  # seconds, rather than cache the reading in the runner or lengthen the
  # timeout. Two reasons it is tolerable. Nothing about a run depends on it: the
  # variant recorded against a run is probed by bin/run.sh at run start and
  # written into run-config.txt, so runs stay correctly labelled while this
  # frame is showing an error. And the failure is loud and honest — it says it
  # could not read the server rather than showing a stale or invented one, which
  # is the property worth keeping if only one of speed and honesty is available.
  #
  # The cost of the decision: while the frame is in that state the switch
  # buttons are not rendered, so changing variant needs a reload first.
  def variant_locals(error: nil)
    { data: client.variants || {}, error: error }
  rescue Harness::Client::Error => e
    { data: {}, error: error || "Could not read the deployed server: #{e.message}" }
  end
end
