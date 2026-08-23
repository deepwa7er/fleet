# Parser for git-format diff text (what the bridge's /change diff endpoints
# return) into files → hunks → numbered lines, so the review can render the
# code itself with the agent's annotations at the point they apply
# (DW-002 §5). Annotations anchor to (path, side, line-number); the line
# numbers computed here are that coordinate system.
#
# DW-001 §8 discipline: this is a plain parser — no rendering, no HTML, no
# markup decisions. It never raises on malformed input: a line it does not
# recognize outside a hunk is skipped, and a truncated hunk simply ends. The
# diff came from jj a moment ago, but the review page must degrade to "what
# it could parse", never to a 500.
class GitDiff
  DiffFile = Struct.new(:old_path, :new_path, :binary, :hunks) do
    # The label a reader sees: the new path, or "old → new" for a rename.
    def label
      return new_path if old_path == new_path

      "#{old_path} → #{new_path}"
    end

    # Whether an annotation on (path, side) belongs to this file.
    def anchors?(path, side)
      side == "old" ? old_path == path : new_path == path
    end
  end

  Hunk = Struct.new(:header, :lines)

  # kind is :context, :add, or :del; the number on the absent side is nil.
  Line = Struct.new(:kind, :old_number, :new_number, :text)

  FILE_HEADER = %r{\Adiff --git a/(?<old>.*) b/(?<new>.*)\z}
  HUNK_HEADER = /\A@@ -(?<old_start>\d+)(?:,\d+)? \+(?<new_start>\d+)(?:,\d+)? @@/

  def self.parse(text)
    files = []
    file = nil
    hunk = nil
    old_number = new_number = nil

    text.to_s.each_line(chomp: true) do |line|
      if (match = FILE_HEADER.match(line))
        file = DiffFile.new(match[:old], match[:new], false, [])
        files << file
        hunk = nil
        next
      end
      next if file.nil?

      if (match = HUNK_HEADER.match(line))
        hunk = Hunk.new(line, [])
        file.hunks << hunk
        old_number = match[:old_start].to_i
        new_number = match[:new_start].to_i
        next
      end

      if line.start_with?("Binary files ")
        file.binary = true
        next
      end
      next if hunk.nil? # file metadata (index, ---/+++, modes, renames)

      case line[0]
      when "+"
        hunk.lines << Line.new(:add, nil, new_number, line[1..])
        new_number += 1
      when "-"
        hunk.lines << Line.new(:del, old_number, nil, line[1..])
        old_number += 1
      when " ", nil
        hunk.lines << Line.new(:context, old_number, new_number, line[1..].to_s)
        old_number += 1
        new_number += 1
      when "\\"
        # "\ No newline at end of file" — a marker, not a line of either side.
      else
        hunk = nil # trailing non-diff content ends the hunk, never crashes it
      end
    end
    files
  end
end
