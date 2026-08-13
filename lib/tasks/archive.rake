namespace :readout do
  namespace :archive do
    desc "Move live run history to cold storage (dump → verify → truncate + VACUUM). Raw results/ stays on disk."
    task cold: :environment do
      manifest = ReadoutArchiver.cold_archive!
      puts "Archived #{manifest[:run_count]} runs (#{manifest[:level_stat_count]} levels, #{manifest[:server_sample_count]} samples) to #{manifest[:dump_path]} (#{manifest[:dump_bytes]} bytes, sha256:#{manifest[:dump_sha256][0..7]})"
      puts "Manifest: storage/archive/#{File.basename(manifest[:dump_path]).sub('.sql.gz', '.json')}"
      puts "Results: #{manifest[:results_archive] || 'no results_root — DB dump only'}"
      puts "Live DB is now empty — no fetch until you restore or re-import."
    end

    desc "Restore the most recent cold archive into the live DB"
    task restore: :environment do
      manifest = ReadoutArchiver.restore_latest!
      puts "Restored #{manifest[:run_count]} runs from #{manifest[:dump_path]}"
      puts "Run `bin/rails runner 'puts Run.count'` to verify."
    end
  end
end
