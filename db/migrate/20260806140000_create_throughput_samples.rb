class CreateThroughputSamples < ActiveRecord::Migration[8.1]
  def change
    create_table :throughput_samples do |t|
      t.references :run, null: false, foreign_key: true
      t.datetime :at
      t.integer :requests

      t.timestamps
    end
  end
end
