class CreateServerSamples < ActiveRecord::Migration[8.1]
  def change
    create_table :server_samples do |t|
      t.references :run, null: false, foreign_key: true
      t.datetime :at
      t.float :cpu_pct
      t.float :mem_pct
      t.float :load1
      t.integer :wal_bytes
      t.integer :db_bytes

      t.timestamps
    end
  end
end
