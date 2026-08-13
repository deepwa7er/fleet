# This file is auto-generated from the current state of the database. Instead
# of editing this file, please use the migrations feature of Active Record to
# incrementally modify your database, and then regenerate this schema definition.
#
# This file is the source Rails uses to define your schema when running `bin/rails
# db:schema:load`. When creating a new database, `bin/rails db:schema:load` tends to
# be faster and is potentially less error prone than running all of your
# migrations from scratch. Old migrations may fail to apply correctly if those
# migrations use external dependencies or application code.
#
# It's strongly recommended that you check this file into your version control system.

ActiveRecord::Schema[8.1].define(version: 2026_08_09_211535) do
  create_table "level_stats", force: :cascade do |t|
    t.float "cable_subscribe_p95"
    t.float "cpu_avg_pct"
    t.float "cpu_peak_pct"
    t.datetime "created_at", null: false
    t.integer "error_count"
    t.float "fanout_p95"
    t.float "history_p95"
    t.float "megabytes_per_second"
    t.float "post_message_p95"
    t.float "requests_per_second"
    t.float "room_open_p95"
    t.integer "run_id", null: false
    t.integer "sample_count"
    t.datetime "updated_at", null: false
    t.integer "vus"
    t.integer "window_seconds"
    t.index ["run_id"], name: "index_level_stats_on_run_id"
  end

  create_table "run_progresses", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.json "payload", null: false
    t.integer "run_id", null: false
    t.datetime "updated_at", null: false
    t.index ["run_id"], name: "index_run_progresses_on_run_id", unique: true
  end

  create_table "runs", force: :cascade do |t|
    t.text "config"
    t.datetime "created_at", null: false
    t.string "generator"
    t.integer "harness_errors"
    t.datetime "imported_at"
    t.string "k6_version"
    t.string "machine"
    t.string "path"
    t.float "peak_cpu_pct"
    t.integer "peak_wal_bytes"
    t.string "scenario"
    t.string "server_digest"
    t.string "server_env"
    t.string "server_image"
    t.string "stamp"
    t.datetime "started_at"
    t.string "target"
    t.integer "total_broadcasts"
    t.integer "total_requests"
    t.integer "unanswered_requests"
    t.datetime "updated_at", null: false
    t.string "variant"
    t.index ["stamp"], name: "index_runs_on_stamp", unique: true
    t.index ["variant"], name: "index_runs_on_variant"
  end

  create_table "server_samples", force: :cascade do |t|
    t.datetime "at"
    t.float "cpu_pct"
    t.datetime "created_at", null: false
    t.integer "db_bytes"
    t.float "load1"
    t.float "mem_pct"
    t.integer "run_id", null: false
    t.datetime "updated_at", null: false
    t.integer "wal_bytes"
    t.index ["run_id"], name: "index_server_samples_on_run_id"
  end

  create_table "throughput_samples", force: :cascade do |t|
    t.datetime "at"
    t.datetime "created_at", null: false
    t.integer "requests"
    t.integer "run_id", null: false
    t.datetime "updated_at", null: false
    t.index ["run_id"], name: "index_throughput_samples_on_run_id"
  end

  add_foreign_key "level_stats", "runs"
  add_foreign_key "run_progresses", "runs"
  add_foreign_key "server_samples", "runs"
  add_foreign_key "throughput_samples", "runs"
end
