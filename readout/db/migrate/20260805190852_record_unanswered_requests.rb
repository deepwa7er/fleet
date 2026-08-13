class RecordUnansweredRequests < ActiveRecord::Migration[8.1]
  def change
    # Requests the application never answered. Deliberately distinct from a
    # request answered with an error: a 500 is the app responding, and silence
    # says something different about a server than a refusal does.
    add_column :runs, :unanswered_requests, :integer

    # failed_requests never held failed requests. It holds campfire_errors — the
    # harness's own counter for a room that would not open, a cable that was
    # rejected, a user in no room of the requested size. Those are conditions the
    # harness noticed, not HTTP failures, and the name invited reading a count of
    # broken requests off a number measuring something else entirely.
    rename_column :runs, :failed_requests, :harness_errors
  end
end
