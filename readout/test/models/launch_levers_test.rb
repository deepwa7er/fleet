require "test_helper"

# What a launch actually asks the harness for.
#
# These are the numbers that decide whether a run is the company it claims to
# be, and one of them was wrong for a release: USER_POOL was capped at 1,000, so
# a 2,000-employee run seeded 2,000 people and then ran 2,000 virtual users
# across 1,000 identities — two per employee — and reported it as 2,000.
class LaunchLeversTest < ActiveSupport::TestCase
  test "every employee gets their own identity, at any company size" do
    [ 10, 300, 2_000 ].each do |size|
      enterprise = TestRunsController.levers_for_enterprise(size)
      chat = TestRunsController.levers_for_chat(size)

      assert_equal size, enterprise["USER_POOL"], "enterprise at #{size}"
      assert_equal size, enterprise["VUS"]
      assert_equal size, chat["USER_POOL"], "chat at #{size}"
      assert_equal size, chat["PEOPLE"]
    end
  end

  # A room every employee belongs to costs a membership row and a broadcast per
  # person in the company, so how often it is written decides what the run
  # measures. At 5% it was producing 99% of the fan-out work.
  test "the all-hands room is written rarely" do
    share = TestRunsController.levers_for_enterprise(300)["HOT_ROOM_SHARE"].to_f

    assert_operator share, :>, 0, "an announcement channel that is never written is not in the test at all"
    assert_operator share, :<=, 0.02, "at more than a couple of percent it stops being an announcement channel"
  end
end
