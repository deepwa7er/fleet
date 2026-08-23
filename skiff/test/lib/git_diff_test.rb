require "test_helper"

# The parser is the annotation coordinate system (path, side, line-number),
# so the numbering assertions here are the contract the review's anchors
# depend on. It must also never raise: malformed tails degrade to "what it
# could parse".
class GitDiffTest < ActiveSupport::TestCase
  SAMPLE = <<~DIFF
    diff --git a/skiff/app/models/harness.rb b/skiff/app/models/harness.rb
    index 111..222 100644
    --- a/skiff/app/models/harness.rb
    +++ b/skiff/app/models/harness.rb
    @@ -10,4 +10,5 @@ class Harness
     unchanged
    -removed line
    +added line
    +another added
     tail line
    @@ -30,2 +31,2 @@
    -old tail
    +new tail
    diff --git a/old name.txt b/new name.txt
    rename from old name.txt
    rename to new name.txt
    @@ -1,1 +1,1 @@
    -before
    +after
    \\ No newline at end of file
    diff --git a/logo.png b/logo.png
    Binary files a/logo.png and b/logo.png differ
  DIFF

  test "parses files, hunks, and both line counters" do
    files = GitDiff.parse(SAMPLE)
    assert_equal 3, files.length

    file = files.first
    assert_equal "skiff/app/models/harness.rb", file.label
    assert_equal 2, file.hunks.length

    lines = file.hunks.first.lines
    assert_equal [ :context, :del, :add, :add, :context ], lines.map(&:kind)
    assert_equal [ 10, 11, nil, nil, 12 ], lines.map(&:old_number)
    assert_equal [ 10, nil, 11, 12, 13 ], lines.map(&:new_number)
    assert_equal "removed line", lines[1].text
    assert_equal "another added", lines[3].text

    second_hunk = file.hunks.last.lines
    assert_equal [ 30, nil ], second_hunk.map(&:old_number)
    assert_equal [ nil, 31 ], second_hunk.map(&:new_number)
  end

  test "renames keep both sides and label with an arrow" do
    file = GitDiff.parse(SAMPLE)[1]
    assert_equal "old name.txt → new name.txt", file.label
    assert file.anchors?("old name.txt", "old")
    assert file.anchors?("new name.txt", "new")
    refute file.anchors?("old name.txt", "new")
    # The no-newline marker is not a line of either side.
    assert_equal %i[del add], file.hunks.first.lines.map(&:kind)
  end

  test "binary files are flagged and hold no hunks" do
    file = GitDiff.parse(SAMPLE)[2]
    assert file.binary
    assert_empty file.hunks
  end

  test "empty and malformed input degrade instead of raising" do
    assert_equal [], GitDiff.parse("")
    assert_equal [], GitDiff.parse(nil)
    assert_equal [], GitDiff.parse("not a diff at all\njust words\n")

    truncated = GitDiff.parse("diff --git a/x b/x\n@@ -1,2 +1,2 @@\n unchanged\ngarbage that ends the hunk\n+never counted\n")
    assert_equal 1, truncated.length
    assert_equal [ :context ], truncated.first.hunks.first.lines.map(&:kind)
  end
end
