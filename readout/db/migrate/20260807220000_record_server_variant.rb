class RecordServerVariant < ActiveRecord::Migration[8.1]
  def change
    # What server produced a run's numbers.
    #
    # Without this a results directory says what load was applied and nothing
    # about what it was applied to, so comparing two runs means knowing out of
    # band which build each one hit. Working that out for a change made by hand
    # meant reading container creation timestamps against an uncommitted working
    # tree — after which the runs still could not say it themselves.
    change_table :runs, bulk: true do |t|
      # The variant name from campfire-stress/variants.json, or "unknown" when
      # the harness could not establish it. Never guessed.
      t.string :variant

      # The image and its digest, so a variant name that was renamed or
      # redefined later still resolves to exactly what ran.
      t.string :server_image
      t.string :server_digest

      # The variables the variant is answerable for, including RAILS_ENV — which
      # decides which database was measured, and whose loss is silent.
      t.string :server_env
    end

    add_index :runs, :variant
  end
end
