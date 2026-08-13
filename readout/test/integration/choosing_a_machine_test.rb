require "test_helper"

# Choosing which machine generates the load.
#
# Load used to come from exactly one box. It can now come from the Mac or the
# wired Fedora desktop, and which one produced a set of numbers changes how they
# read: the desktop shares a LAN with the Campfire host, while the Mac is on
# Wi-Fi, where a ~400KB room page can saturate the link before the application
# does. The rules below are what keep that choice honest.
class ChoosingAMachineTest < ActionDispatch::IntegrationTest
  MAC = "http://10.0.0.1:7881".freeze
  DESKTOP = "http://10.0.0.2:7881".freeze

  MACHINES = [
    { "key" => "mac", "name" => "MacBook", "url" => MAC },
    { "key" => "desktop", "name" => "Fedora desktop", "url" => DESKTOP }
  ].freeze

  # A machine that is powered off or asleep.
  #
  # Modelled the way the real client behaves rather than by raising on
  # construction: Harness::Client.new always succeeds — it only parses an
  # address — and #health turns every connection failure into nil so that a
  # missing machine can never take a page down.
  class UnreachableRunner
    def health = nil
    def run(_id) = raise(Harness::Client::Error, "unreachable")
    def progress(_id) = raise(Harness::Client::Error, "unreachable")
    def log(_id) = raise(Harness::Client::Error, "unreachable")
    def variants = raise(Harness::Client::Error, "unreachable")
  end

  # Answers for one machine.
  class FakeRunner
    attr_reader :started

    def initialize(machine:, busy_with: nil)
      @machine = machine
      @busy_with = busy_with
    end

    def health
      { "ok" => true, "busy" => @busy_with.present?, "machine" => @machine }
        .merge(@busy_with ? { "active_run" => @busy_with } : {})
    end

    def start(scenario:, levers:, note: nil)
      @started = { scenario: scenario, levers: levers, note: note }
      { "id" => "20260101-120000" }
    end

    def run(id)
      return { "id" => id, "state" => "running", "machine" => @machine } if id == @busy_with

      raise Harness::Client::Error, "no such run"
    end

    def variants = { "variants" => [], "current" => "stock" }
    def progress(_id) = nil
    def log(_id) = ""
  end

  # `config` is positional rather than a keyword on purpose: with any keyword
  # parameter present, Ruby routes a braceless trailing hash to it, and every
  # call here passes the reachable machines exactly that way.
  def with_machines(reachable, config = MACHINES)
    previous = Rails.configuration.x.runners
    Rails.configuration.x.runners = config

    # Dispatched on address, so one machine can answer while another is
    # unreachable — the ordinary state when the desktop is powered off.
    Harness::Client.define_singleton_method(:new) do |url:, token_file: nil|
      reachable.fetch(url) { UnreachableRunner.new }
    end

    yield
  ensure
    Rails.configuration.x.runners = previous
    Harness::Client.singleton_class.remove_method(:new)
  end

  test "offers every machine that is answering" do
    with_machines(MAC => FakeRunner.new(machine: "mac"),
                  DESKTOP => FakeRunner.new(machine: "desktop")) do
      get new_test_run_path

      assert_response :success
      assert_select "input[type=radio][name=machine][value=mac]"
      assert_select "input[type=radio][name=machine][value=desktop]"
      assert_select "legend", text: "Generate the load from"
    end
  end

  # A machine that is powered off must simply not be on the list. It is the
  # ordinary state of the desktop, and an option that cannot work is worse than
  # no option at all.
  test "leaves out a machine that is not answering" do
    with_machines(MAC => FakeRunner.new(machine: "mac")) do
      get new_test_run_path

      assert_response :success
      assert_select "input[type=radio][name=machine][value=mac]"
      assert_select "input[type=radio][name=machine][value=desktop]", count: 0
    end
  end

  test "runs on the machine that was chosen" do
    desktop = FakeRunner.new(machine: "desktop")

    with_machines(MAC => FakeRunner.new(machine: "mac"), DESKTOP => desktop) do
      post test_runs_path, params: { machine: "desktop", scenario: "chat", people: "50" }

      assert_redirected_to run_path("20260101-120000")
      assert_equal "chat", desktop.started[:scenario]
      assert_equal "50", desktop.started[:levers]["PEOPLE"]
    end
  end

  # The rule no single runner can enforce. Each refuses a second run within its
  # own process, but they cannot see each other — and every generator points at
  # the same Campfire, so a run on one machine would measure the load the other
  # is generating as well as the server's response to it.
  test "refuses a run on one machine while another machine is running one" do
    desktop = FakeRunner.new(machine: "desktop")

    with_machines(MAC => FakeRunner.new(machine: "mac", busy_with: "20260101-110000"),
                  DESKTOP => desktop) do
      post test_runs_path, params: { machine: "desktop", scenario: "chat", people: "50" }

      assert_redirected_to new_test_run_path
      assert_match(/MacBook is already running 20260101-110000/, flash[:alert])
      assert_nil desktop.started, "the second run must not have been started"
    end
  end

  test "names the machine a running test is on" do
    with_machines(MAC => FakeRunner.new(machine: "mac"),
                  DESKTOP => FakeRunner.new(machine: "desktop", busy_with: "20260101-110000")) do
      get runs_path

      assert_response :success
      assert_select "section.verdict strong", text: /running on Fedora desktop/
    end
  end

  # The dashboard reaches runners by IP, because a container on the VPS cannot
  # resolve MagicDNS. Tailnet addresses are not constants, so an entry can end up
  # pointing at a different box — which answers perfectly happily. Launching
  # there would record a run on one machine while the picker claimed another.
  test "will not offer a machine answering under another machine's name" do
    with_machines(MAC => FakeRunner.new(machine: "mac"),
                  DESKTOP => FakeRunner.new(machine: "mac")) do
      get new_test_run_path

      assert_response :success
      assert_select "input[type=radio][name=machine][value=mac]"
      assert_select "input[type=radio][name=machine][value=desktop]", count: 0
    end
  end

  # And says so, rather than telling someone to start a runner they are already
  # running — which would send them looking in entirely the wrong place.
  test "explains an address that points at the wrong machine" do
    with_machines({ DESKTOP => FakeRunner.new(machine: "mac") }, [ MACHINES.last ]) do
      get new_test_run_path

      assert_redirected_to runs_path
      assert_match(/answers as "mac"/, flash[:alert])
    end
  end

  test "refuses a machine it does not know" do
    with_machines(MAC => FakeRunner.new(machine: "mac")) do
      post test_runs_path, params: { machine: "toaster", scenario: "chat", people: "50" }

      assert_redirected_to new_test_run_path
      assert_match(/No such machine/, flash[:alert])
    end
  end

  # A runner started without --machine is older than this feature, not broken.
  # Refusing it would be inventing a fault.
  test "accepts a machine that does not name itself" do
    with_machines(MAC => FakeRunner.new(machine: nil)) do
      get new_test_run_path

      assert_response :success
      assert_select "input[type=radio][name=machine][value=mac]"
    end
  end
end
