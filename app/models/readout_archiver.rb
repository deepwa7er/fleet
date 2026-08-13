class ReadoutArchiver
  class ArchiveError < StandardError; end

  # Where cold dumps live on the dev box. This is the same volume that
  # Analysis::Importer reads from, so the archive sits next to the source
  # results and is included in fleet-backup. Nothing here is ever fetched
  # while cold — it is only read when you explicitly restore.
  def self.archive_root
    Rails.root.join("storage", "archive")
  end

  def self.db_path
    ActiveRecord::Base.connection_db_config.database
  end

  # Dump → verify → truncate. The dump is the history; the live DB is the
  # warm view. Verification happens in a throwaway copy so a corrupt gzip
  # never reaches the archive.
  def self.cold_archive!
    run_count = Run.count
    raise ArchiveError, "Nothing to archive — no runs in #{db_path}" if run_count.zero?

    archive_root.mkpath
    stamp = Time.current.strftime("%Y%m%d-%H%M%S")
    dump_path = archive_root.join("readout-#{stamp}.sql.gz")
    manifest_path = archive_root.join("readout-#{stamp}.json")
    verify_path = Rails.root.join("tmp", "verify-#{stamp}.sqlite3")

    # 1. Dump the live DB that ActiveRecord is actually using. Using the
    # sqlite3 CLI keeps the dump restorable even if the schema changes;
    # ActiveRecord's schema.rb does not contain data.
    dump_ok = system("bash", "-lc", "sqlite3 #{db_path} .dump | gzip > #{dump_path}")
    raise ArchiveError, "Dump failed for #{db_path}" unless dump_ok && dump_path.exist? && dump_path.size.positive?

    # 2. Verify by restoring into a throwaway file and counting rows.
    FileUtils.rm_f(verify_path)
    verify_ok = system("bash", "-lc", "gzip -dc #{dump_path} | sqlite3 #{verify_path} 2>&1 | head -n 20")
    raise ArchiveError, "Verify restore failed" unless verify_ok && verify_path.exist?

    verify_count = `sqlite3 #{verify_path} "SELECT count(*) FROM runs;" 2>&1`.strip.to_i

    FileUtils.rm_f(verify_path)

    unless verify_count == run_count
      raise ArchiveError, "Verify mismatch — live #{run_count} vs dump #{verify_count}"
    end

    sha = `sha256sum #{dump_path} 2>&1`.split.first

    # 3. Also snapshot the raw results directory if present. The DB is
    # derived from results/, so results/ is the canonical history. Keeping
    # both means you can restore from either the dump or a re-import.
    results_root = Rails.configuration.x.campfire_stress.results_root
    results_archive = nil
    if results_root.present? && Dir.exist?(results_root)
      results_archive = archive_root.join("results-#{stamp}.tar.gz")
      system("tar", "-czf", results_archive.to_s, "-C", File.dirname(results_root), File.basename(results_root))
    end

    manifest = {
      created_at: Time.current.iso8601,
      stamp: stamp,
      db_path: db_path,
      dump_path: dump_path.to_s,
      dump_sha256: sha,
      dump_bytes: dump_path.size,
      run_count: run_count,
      level_stat_count: LevelStat.count,
      server_sample_count: ServerSample.count,
      results_root: results_root,
      results_archive: results_archive&.to_s
    }

    File.write(manifest_path, JSON.pretty_generate(manifest))

    # 4. Only now truncate the live tables. This is the warm → cold transition.
    # LevelStats and ServerSamples are deleted via Run's dependent: :delete_all,
    # but clearing them explicitly makes VACUUM reclaim the pages sooner.
    Run.transaction do
      ServerSample.delete_all
      LevelStat.delete_all
      Run.delete_all
    end

    # Reclaim the pages the deleted rows held. Without VACUUM the file stays
    # the same size and the next import merely reuses the free list.
    system("sqlite3", db_path, "VACUUM;")

    manifest
  end

  # Restore the most recent cold dump. The live DB is replaced wholesale;
  # if you prefer a re-derive from results/, use Analysis::Importer directly.
  def self.restore_latest!
    latest = Dir[archive_root.join("readout-*.json")].max
    raise ArchiveError, "No archive found in #{archive_root}" if latest.nil?

    manifest = JSON.parse(File.read(latest))
    dump_path = manifest["dump_path"]

    raise ArchiveError, "Dump missing: #{dump_path}" unless File.exist?(dump_path)

    # Replace the live DB file while the app is running by restoring into a
    # temporary file and then atomically moving it. This avoids a half-written
    # file if the gzip stream is corrupt.
    tmp = Rails.root.join("tmp", "restore-#{Time.current.strftime("%Y%m%d-%H%M%S")}.sqlite3")
    FileUtils.rm_f(tmp)

    restore_ok = system("bash", "-lc", "gzip -dc #{dump_path} | sqlite3 #{tmp} 2>&1 | head -n 20")
    raise ArchiveError, "Restore failed for #{dump_path}" unless restore_ok && tmp.exist?

    # Verify before swapping.
    restored = `sqlite3 #{tmp} "SELECT count(*) FROM runs;" 2>&1`.strip.to_i
    expected = manifest["run_count"].to_i
    raise ArchiveError, "Restore mismatch — dump #{restored} vs manifest #{expected}" unless restored == expected

    # Atomic swap. ActiveRecord will pick up the new file on next checkout;
    # existing connections that already hold the old file descriptor keep it
    # until they are recycled, which is why the health check will briefly show
    # the old count but recovers on next request.
    FileUtils.mv(tmp, db_path)

    manifest
  end
end
