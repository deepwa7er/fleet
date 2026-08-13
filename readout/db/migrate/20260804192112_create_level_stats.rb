class CreateLevelStats < ActiveRecord::Migration[8.1]
  def change
    create_table :level_stats do |t|
      t.references :run, null: false, foreign_key: true
      t.integer :vus
      t.float :room_open_p95
      t.float :history_p95
      t.float :post_message_p95
      t.float :fanout_p95
      t.float :cable_subscribe_p95
      t.float :requests_per_second
      t.float :megabytes_per_second
      t.integer :error_count
      t.float :cpu_avg_pct
      t.float :cpu_peak_pct
      t.integer :window_seconds
      t.integer :sample_count

      t.timestamps
    end
  end
end
