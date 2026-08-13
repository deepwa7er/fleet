class MessagesController < ApplicationController
  # DW-001 §8 discipline: sending is a client call that can fail — the opencode
  # server is a separate process. Blank input never reaches the client, and a
  # failure redirects back with a danger flash so the composer text survives
  # the round trip.
  def create
    text = params[:message].to_s
    return redirect_to session_path(params[:id]) if text.blank?

    OpencodeClient.prompt_async(params[:id], text)
    redirect_to session_path(params[:id])
  rescue OpencodeClient::Error
    redirect_to session_path(params[:id]), alert: "Could not send — opencode server unreachable"
  end
end
