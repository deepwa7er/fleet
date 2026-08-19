class MessagesController < ApplicationController
  # DW-001 §8 discipline: sending is a client call that can fail — the skiff
  # bridge is a separate process. Blank input never reaches the client, and a
  # failure redirects back with a danger flash so the composer text survives
  # the round trip.
  def create
    text = params[:message].to_s
    return redirect_to session_path(params[:id]) if text.blank?

    BridgeClient.prompt_async(params[:id], text)
    redirect_to session_path(params[:id])
  rescue BridgeClient::Error
    redirect_to session_path(params[:id]), alert: "Could not send — skiff bridge unreachable"
  end
end
