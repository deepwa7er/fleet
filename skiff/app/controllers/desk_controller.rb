# The root page (DW-002 §6): one page ordered by what needs you. Sessions
# and changes are different objects — a session is an open-ended
# conversation, a change is a unit of work with an ending — but object type
# is the wrong way to organize the page, so it renders registers instead:
#
#   needs you   changes in review (and landing) — the only count that matters
#   working     sessions with a live run, changes an agent is revising
#   idle        quiet sessions, landed changes
#
# Everything degrades the way sessions#index does: an unreachable bridge is
# a named state on a page that still renders, never a 500. The changes and
# sessions fetches fail independently — a bridge with a broken change
# subsystem (no jj on this host) still shows the session registers.
class DeskController < ApplicationController
  def index
    @changes = []
    @sessions = []
    @harness_errors = {}
    @busy_ids = []
    @error = nil
    @changes_error = nil

    begin
      payload = BridgeClient.sessions
      @sessions = Array(payload[:sessions]).sort_by { |session| -last_activity(session) }
      @harness_errors = payload[:errors] || {}
      @busy_ids = BridgeClient.session_statuses.keys.map(&:to_s)
    rescue BridgeClient::Error
      @error = "skiff bridge unreachable — start it and reload"
    end

    begin
      @changes = Array(BridgeClient.changes[:changes]) if @error.nil?
    rescue BridgeClient::Error => e
      @changes_error = e.remote_message || "changes unavailable"
    end

    @needs_you = @changes.select { |change| %w[in_review landing].include?(change[:state]) }
    working_changes = @changes.select { |change| change[:state] == "working" }
    busy = @sessions.select { |session| @busy_ids.include?(session[:id].to_s) }
    idle_sessions = @sessions - busy
    @working = { changes: working_changes, sessions: busy }
    @idle = { changes: @changes.select { |change| change[:state] == "shipped" }, sessions: idle_sessions }
  end

  private

  def last_activity(session)
    session.dig(:time, :updated) || session.dig(:time, :created) || 0
  end
end
