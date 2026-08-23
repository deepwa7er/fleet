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
