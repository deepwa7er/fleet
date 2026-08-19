# Loads the skiff secrets file into ENV.
#
# DW-001 §8 discipline: this initializer serves one consumer — the skiff
# bridge password (SKIFF_BRIDGE_PASSWORD) that the Rails client sends to
# the bridge on 127.0.0.1:4120. The app must not hard-code
# credentials, and tests/CI must be able to boot without the real secrets file
# present. So the file is optional: if it is missing we simply leave ENV
# untouched (tests set ENV directly).
#
# DW-001 §8 portability: the secrets path is a per-user default resolved from
# the home directory rather than a hard-coded absolute path, because the app
# deploys to two hosts — the Mac (launchd) and the Fedora desktop (systemd) —
# which run under different users with different homes. Dir.home keeps the
# file at ~/.config/skiff/secrets on both, and SKIFF_SECRETS_FILE overrides
# the location for any layout that puts it elsewhere.
#
# Format: env-style lines `KEY=VALUE`, `#` comments and blank lines skipped.
# Values are taken verbatim, so no shell expansion or interpolation applies.
SECRETS_FILE = ENV.fetch("SKIFF_SECRETS_FILE", File.join(Dir.home, ".config", "skiff", "secrets"))

if File.exist?(SECRETS_FILE)
  File.readlines(SECRETS_FILE).each do |line|
    line = line.strip
    next if line.empty? || line.start_with?("#")

    key, value = line.split("=", 2)
    ENV[key] = value if key && value
  end
end
