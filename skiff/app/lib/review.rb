# The review's view-model (DW-002 §5–6): pick the round to show and parse
# its diff. Shared by the change page and the session page's embedded
# review, so the two views of one change can never disagree about what a
# round is.
#
# round: nil → the latest round (the session embed's view); "all" → the
# cumulative view (round nil — annotations have no coordinates there); a
# number → that round, falling back to the latest. The diff fetch is a
# bridge call and can fail like any other; callers rescue BridgeClient::Error
# around #for.
class Review
  attr_reader :change, :round, :diff_files

  def self.for(change, round: nil)
    new(change, round)
  end

  def initialize(change, requested)
    @change = change
    rounds = Array(change[:rounds])
    @round =
      if requested == "all" || rounds.empty?
        nil
      else
        rounds.find { |r| r[:n] == requested.to_i } || rounds.last
      end
    @diff_files =
      if rounds.empty?
        []
      else
        GitDiff.parse(BridgeClient.change_diff(change[:repo], change[:card], round: @round&.[](:n))[:diff])
      end
  end
end
