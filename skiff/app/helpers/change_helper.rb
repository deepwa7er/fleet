# Presentation for the review (DW-002 §5): titles and instrumentation lines
# for change objects, and the placement of annotations into a parsed diff.
# All voice rules are DW-001's — facts joined with ·, readouts in the
# instrumentation recipe, never prose where a fact will do.
module ChangeHelper
  def change_title(change)
    change[:title].presence || "change ##{change[:card]}"
  end

  # "fleet #81 · round 2 · in review · 3m" — each fact skipped cleanly when
  # absent, like the session lines.
  def change_instrumentation(change)
    rounds = Array(change[:rounds]).length
    [
      "#{change[:repo]} ##{change[:card]}",
      (rounds.positive? ? "round #{rounds}" : "no rounds"),
      change[:state].to_s.tr("_", " "),
      relative_change_activity(change)
    ].compact.join(" · ")
  end

  # The header carries the agent's report as a claim, labelled as one
  # (DW-002 §5): "agent ran cargo test · clippy". Nothing here checked
  # anything, and the copy must keep saying so.
  def round_claims(round)
    gates = Array(round[:gatesRan])
    return nil if gates.empty?

    "agent ran #{gates.join(" · ")}"
  end

  # Index annotations by their anchor so the diff renders each at the point
  # it applies: {[path, side, line] => [annotation, …]}. Second return is
  # the annotations no rendered line anchors — a round rewritten by conflict
  # resolution can strand one, and a stranded annotation is shown, labelled,
  # never dropped.
  def place_annotations(round, diff_files)
    lookup = Hash.new { |hash, key| hash[key] = [] }
    Array(round&.[](:annotations)).each do |annotation|
      lookup[[ annotation[:path], annotation[:side], annotation[:line] ]] << annotation
    end
    placed = {}
    diff_files.each do |file|
      file.hunks.each do |hunk|
        hunk.lines.each do |line|
          anchors_for(file, line).each do |key|
            placed[key] = true if lookup.key?(key)
          end
        end
      end
    end
    stranded = lookup.reject { |key, _| placed[key] }.values.flatten
    [ lookup, stranded ]
  end

  # The annotations sitting on one rendered diff line: an added line anchors
  # the new side, a deleted line the old side, a context line both.
  def annotations_at(lookup, file, line)
    anchors_for(file, line).flat_map { |key| lookup.fetch(key, []) }
  end

  # One service's deploy state, as a fact: the daemon reported it already
  # deploying (this approval started nothing), a job is still running, it
  # deployed, or it failed with the daemon's reason.
  def deploy_service_state(service)
    return "already deploying" if service[:status] == "in_progress"

    outcome = service[:outcome]
    return "deploying" if outcome.nil?

    outcome[:ok] ? "deployed" : "failed — #{outcome[:message]}"
  end

  # Whether a shipped change still has deploy jobs in flight — true from the
  # trigger until every started job reports an outcome (or the bridge's poll
  # deadline passed). Keeps the review page reloading while the fleet is
  # still deploying, exactly like a landing in flight.
  def deploy_pending?(change)
    deploy = change[:deploy]
    return false if deploy.nil? || deploy[:error]

    Array(deploy[:services]).any? { |service| service[:jobId] && service[:outcome].nil? }
  end

  # The one-line readout for the fleet deploy an approval triggered (card
  # #122): "deploy in progress · n services" while any job is in flight,
  # then the terminal "deploy complete" once every outcome is in — with a
  # failure count when any service failed. nil when there is nothing to say
  # (no deploy recorded, the trigger failed, or nothing was started): the
  # trigger failure has its own line, and the per-service lines carry the
  # detail beneath the summary.
  def deploy_readout(change)
    deploy = change[:deploy]
    return nil if deploy.nil? || deploy[:error]

    services = Array(deploy[:services])
    return nil if services.empty?

    if deploy_pending?(change)
      { text: "deploy in progress · #{pluralize(services.length, "service")}", failed: false }
    else
      failed = services.count { |service| service[:outcome] && !service[:outcome][:ok] }
      {
        text: failed.zero? ? "deploy complete" : "deploy complete · #{pluralize(failed, "service")} failed",
        failed: failed.positive?
      }
    end
  end

  # The GitHub commit approve pushed to main (DW-002 §6), once the push has
  # actually happened. nil before the landing completes — and for the
  # bridge's degraded "(unresolved)" tip — so a missing commit never links
  # to a made-up URL.
  def landed_commit_url(change)
    tip = change.dig(:landed, :tip)
    return nil if tip.blank? || tip == "(unresolved)"

    "https://github.com/deepwa7er/#{change[:repo]}/commit/#{tip}"
  end

  private

  def anchors_for(file, line)
    keys = []
    keys << [ file.new_path, "new", line.new_number ] if line.new_number
    keys << [ file.old_path, "old", line.old_number ] if line.old_number
    keys
  end

  def relative_change_activity(change)
    timestamp = change[:updatedAt]
    return nil if timestamp.blank?

    time_ago_in_words(Time.iso8601(timestamp))
  rescue ArgumentError
    nil
  end
end
