# The review (DW-002 §5–6): the annotated change, and the verbs that close
# the loop without navigating anywhere. GETs degrade like every other page
# (an unreachable bridge is a named state); the verb actions follow the
# sessions pattern — validate, one BridgeClient call, redirect with a flash
# — but their rescue distinguishes the bridge's own refusal (a 409 with a
# reason worth reading) from an unreachable bridge, via Error#status.
class ChangesController < ApplicationController
  # The review page. ?round=n shows that round's diff (what changed since
  # you last looked); ?round=all shows the cumulative diff (the feature as
  # it now stands); the default is the latest round. Annotations render
  # inline only in a round view — they are positioned in a round's diff and
  # have no coordinates in the cumulative one.
  def show
    @change = BridgeClient.change(params[:repo], params[:card])
    @review = Review.for(@change, round: params[:round])
    load_bound_session
  rescue BridgeClient::Error => e
    @error = e.status ? (e.remote_message || "the bridge refused: HTTP #{e.status}") : "skiff bridge unreachable — start it and reload"
  end

  # Poll target for the page's reload trigger (change-poll controller): the
  # few fields whose movement means the page is stale. The view renders ops
  # and reloads whole — it never diffs (skiff's own rule).
  def status
    change = BridgeClient.change(params[:repo], params[:card])
    render json: {
      state: change[:state],
      rounds: Array(change[:rounds]).length,
      updatedAt: change[:updatedAt],
      deployPending: helpers.deploy_pending?(change)
    }
  rescue BridgeClient::Error
    head :bad_gateway
  end

  # Verb one. The bridge answers 202 and lands async; the page's poll shows
  # the outcome (shipped, or back in review carrying the reason). A `session`
  # param means the verb came from that session page's embedded review — the
  # loop stays in the chat it started from, so the redirect goes back there.
  def approve
    BridgeClient.approve_change(params[:repo], params[:card])
    redirect_to verb_target, notice: "Landing — this page will follow the outcome."
  rescue BridgeClient::Error => e
    redirect_to verb_target, alert: verb_failure(e, "Could not approve")
  end

  # Verb two. The note goes to the bound agent session and the change
  # reopens; round n+1 appears in place when the agent finishes.
  def request_changes
    note = params[:note].to_s.strip
    if note.blank?
      return redirect_to verb_target, alert: "Type the note first — it becomes the agent's next round."
    end

    BridgeClient.request_changes(params[:repo], params[:card], note)
    redirect_to verb_target, notice: "Sent — the agent is working; the next round will appear here."
  rescue BridgeClient::Error => e
    redirect_to verb_target, alert: verb_failure(e, "Could not send the note")
  end

  private

  # Where a verb returns the reader: the session page that embedded the
  # review when the verb came from there (the loop closes in the chat),
  # otherwise the change page itself.
  def verb_target
    session = params[:session].to_s
    if session.present?
      session_path(session)
    else
      change_path(params[:repo], params[:card])
    end
  end

  # The loop closes in one view: when the change has a bound session, the
  # review embeds its live transcript. A session fetch that fails degrades
  # to the review without the embed — the diff is still the point.
  def load_bound_session
    @session = nil
    @messages = []
    return if @change[:session].blank?

    @session = BridgeClient.session(@change[:session])
    @messages = Array(BridgeClient.messages(@change[:session]))
  rescue BridgeClient::Error
    @session = nil
    @messages = []
  end

  # A refusal with a reason renders the reason; a dead socket renders the
  # uniform unreachable line. Never the raw HTTP detail.
  def verb_failure(error, prefix)
    if error.status
      "#{prefix} — #{error.remote_message || "the bridge refused (HTTP #{error.status})"}"
    else
      "#{prefix} — skiff bridge unreachable"
    end
  end
end
