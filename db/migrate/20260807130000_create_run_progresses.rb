class CreateRunProgresses < ActiveRecord::Migration[8.1]
  def change
    create_table :run_progresses do |t|
      # One payload per run, and the index says so: importing a run twice must
      # replace its charts rather than leave the page a choice of two.
      t.references :run, null: false, foreign_key: true, index: { unique: true }

      # Stored as the document it is. Every read wants the whole thing — it is
      # handed to the chart verbatim — so there is nothing to gain from breaking
      # a few thousand points into rows.
      t.json :payload, null: false

      t.timestamps
    end
  end
end
