# Builds a campfire-stress results directory on the fly.
#
# Written rather than committed as fixture files so the shape of the input is
# visible in the tests that use it — and so the suite never depends on an 80MB
# artifact.
#
# Shared because two things read these directories now: Analysis::RunBundle
# parses one, and Analysis::Importer stores what it produced. They must be
# exercised against the same input or the seam between them is untested.
module ResultsDirectory
  METRICS_HEADER = "metric_name,timestamp,metric_value,check,error,error_code," \
                   "expected_response,group,method,name,proto,scenario,service," \
                   "status,subproto,tls_version,url,extra_tags,metadata".freeze

  def write_run(dir, stamp:, base: 1_700_000_000, machine: nil)
    run_dir = File.join(dir, stamp)
    FileUtils.mkdir_p(run_dir)

    rows = [ METRICS_HEADER ]
    server = [ "ts,cpu_pct,mem_pct,load1,wal_bytes,db_bytes" ]

    # Two plateaus, 60s each, with a level revisited on the way down so the
    # contiguous-segment rule is genuinely exercised.
    schedule = ([ 10 ] * 60) + ([ 20 ] * 60) + ([ 10 ] * 40)

    schedule.each_with_index do |vus, offset|
      at = base + offset
      rows << "vus,#{at},#{vus},,,,,,,,,,,,,,,,"
      rows << "campfire_room_open,#{at},#{vus * 10},,,,true,,GET,room,,,,200,,,,endpoint=room_open,"
      rows << "http_reqs,#{at},1,,,,true,,GET,room,,,,200,,,,endpoint=room_open,"
      rows << "data_received,#{at},#{1_048_576},,,,,,,,,,,,,,,,"
      server << "#{at},#{vus * 20}.0,5.0,0.5,#{1024 * offset},2048"
    end

    File.write(File.join(run_dir, "metrics.csv"), rows.join("\n") + "\n")
    File.write(File.join(run_dir, "server.csv"), server.join("\n") + "\n")
    File.write(File.join(run_dir, "run-config.txt"), <<~CONFIG)
      stamp=#{stamp}
      scenario=scenarios/ramp.js
      target=http://example.test
      generator=test-host
      #{"machine=#{machine}" if machine}
      k6=k6 v2.1.0
      VUS=<config default>
      USER_POOL=800
    CONFIG

    run_dir
  end
end
