class CreateRuns < ActiveRecord::Migration[8.1]
  def change
    create_table :runs do |t|
      t.string :stamp
      t.string :path
      t.string :scenario
      t.string :target
      t.string :generator
      t.string :k6_version
      t.datetime :started_at
      t.datetime :imported_at
      t.float :peak_cpu_pct
      t.integer :peak_wal_bytes
      t.integer :total_broadcasts
      t.integer :total_requests
      t.integer :failed_requests
      t.text :config

      t.timestamps
    end
    add_index :runs, :stamp, unique: true
  end
end
