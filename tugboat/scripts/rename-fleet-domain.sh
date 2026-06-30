#!/usr/bin/env bash
#
# rename-fleet-domain.sh — move the deepwa7er fleet to a new domain.
#
# The fleet's domain lives in two structurally different strings:
#
#   * the base domain — the subdomain space services live under, e.g.
#       internal.deepwa7er.com  →  <service>.internal.deepwa7er.com
#   * the DNS zone — the registrable apex Cloudflare hosts, used for the ACME
#       DNS-01 challenge, e.g.  deepwa7er.com
#
# They are nested (the zone is a suffix of the base) but are genuinely different
# settings, so both are passed and rewritten. This script is a CODE edit only:
# it rewrites every occurrence across the fleet repos and nothing else. DNS, the
# Cloudflare zone/token, the harbor extension auto-update channel, the iOS app
# rebuilds, and the redeploys are all manual — see the CHECKLIST it prints.
#
# It only ever rewrites the two domain strings you pass. It never touches the
# bare host/tailnet name `deepwa7er` (the SSH alias and Tailscale node name are
# independent of the public domain).
#
# Usage:
#   rename-fleet-domain.sh OLD_BASE NEW_BASE OLD_ZONE NEW_ZONE [--apply]
#
# Dry-run (the default — prints every file and how many replacements it would
# make, changing nothing):
#   rename-fleet-domain.sh internal.deepwa7er.com internal.example.net \
#                          deepwa7er.com example.net
#
# Add --apply to write the changes. Re-run the dry-run afterwards to confirm
# zero remaining occurrences.
#
# Override the repo root with FLEET_ROOT (defaults to ~/code).

set -euo pipefail

if [ "$#" -lt 4 ] || [ "$#" -gt 5 ]; then
  grep '^#' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
fi

OLD_BASE="$1"
NEW_BASE="$2"
OLD_ZONE="$3"
NEW_ZONE="$4"
MODE="dry"
if [ "${5:-}" = "--apply" ]; then
  MODE="apply"
elif [ "$#" -eq 5 ]; then
  echo "error: unknown 5th argument ${5:?}; expected --apply" >&2
  exit 2
fi

for pair in "OLD_BASE:$OLD_BASE" "NEW_BASE:$NEW_BASE" "OLD_ZONE:$OLD_ZONE" "NEW_ZONE:$NEW_ZONE"; do
  if [ -z "${pair#*:}" ]; then
    echo "error: ${pair%%:*} must not be empty" >&2
    exit 2
  fi
done
if [ "$OLD_BASE" = "$NEW_BASE" ] && [ "$OLD_ZONE" = "$NEW_ZONE" ]; then
  echo "error: old and new domains are identical — nothing to do" >&2
  exit 2
fi

ROOT="${FLEET_ROOT:-$HOME/code}"

# The fleet repos to scan — mirrors tugboat/fleet.toml's members plus ferry-config
# (the live ferry config repo). Kept explicit so unrelated projects under the
# root (playground, poe2-mcp, siren, …) are never touched.
FLEET_DIRS=(
  lighthouse breakwater ferry ferry-config tidepool harbor lagoon source
  driftword drydock tide pilot sonar fleet-backup git-autocommit tugboat Helm
)

# Skip build outputs, dependencies, VCS metadata, lockfiles, and this script
# itself (its usage examples contain the old domain and must not be rewritten).
EXCLUDES=(
  --exclude-dir=.git --exclude-dir=target --exclude-dir=node_modules
  --exclude-dir=dist --exclude-dir=.build --exclude-dir=DerivedData
  --exclude='*.lock' --exclude='Cargo.lock' --exclude='rename-fleet-domain.sh'
)

# Collect every file under the fleet repos that mentions either domain string.
files=()
while IFS= read -r f; do
  [ -n "$f" ] && files+=("$f")
done < <(
  for d in "${FLEET_DIRS[@]}"; do
    if [ ! -d "$ROOT/$d" ]; then
      echo "  (skipping missing repo: $d)" >&2
      continue
    fi
    grep -rIl "${EXCLUDES[@]}" -e "$OLD_BASE" -e "$OLD_ZONE" "$ROOT/$d" 2>/dev/null || true
  done | sort -u
)

echo
if [ "$MODE" = "apply" ]; then
  echo "Rewriting $OLD_BASE → $NEW_BASE  and  $OLD_ZONE → $NEW_ZONE"
else
  echo "DRY RUN — would rewrite $OLD_BASE → $NEW_BASE  and  $OLD_ZONE → $NEW_ZONE"
  echo "(re-run with --apply to write)"
fi
echo

if [ "${#files[@]}" -eq 0 ]; then
  echo "No occurrences found under $ROOT — nothing to do."
  exit 0
fi

# Do the replacement (or count it) with perl: base first, then zone. Because the
# base string contains the zone as a suffix, replacing the base first means the
# zone pass only ever matches standalone zone references — no double-rewrite.
OLD_BASE="$OLD_BASE" NEW_BASE="$NEW_BASE" OLD_ZONE="$OLD_ZONE" NEW_ZONE="$NEW_ZONE" MODE="$MODE" \
perl -e '
  use strict; use warnings;
  my ($ob,$nb,$oz,$nz,$mode) = @ENV{qw/OLD_BASE NEW_BASE OLD_ZONE NEW_ZONE MODE/};
  my ($files, $grand) = (0, 0);
  for my $f (@ARGV) {
    local $/;
    open my $fh, "<", $f or do { warn "  skip (read) $f: $!\n"; next };
    my $c = <$fh>; close $fh;
    my $n = 0;
    $n += ($c =~ s/\Q$ob\E/$nb/g);
    $n += ($c =~ s/\Q$oz\E/$nz/g);
    next unless $n;
    $files++; $grand += $n;
    printf "  %4d  %s\n", $n, $f;
    if ($mode eq "apply") {
      open my $out, ">", $f or do { warn "  skip (write) $f: $!\n"; next };
      print $out $c; close $out;
    }
  }
  printf "\n%s %d replacement(s) across %d file(s).\n",
    ($mode eq "apply" ? "Applied" : "Would make"), $grand, $files;
' "${files[@]}"

cat <<CHECKLIST

────────────────────────────────────────────────────────────────────────────
MANUAL STEPS (the script does NOT do these — code edits alone won't move the
fleet). Do them in this order:

1. DNS — in the NEW_ZONE Cloudflare zone, create the same record that makes
   *.${OLD_BASE} resolve today (a wildcard for *.${NEW_BASE} pointing at the
   tailnet IP 100.98.184.58). Verify with: dig +short test.${NEW_BASE}

2. Cloudflare — ${NEW_ZONE} must be a zone in the same Cloudflare account, and
   the API token at /etc/breakwater/cloudflare-token (on deepwa7er) must have
   DNS:Edit + Zone:Read for it.

3. ACME — breakwater issues a fresh wildcard cert for *.${NEW_BASE} via DNS-01
   on first deploy. Consider pointing [acme] directory at the Let's Encrypt
   STAGING URL for one deploy first to avoid rate limits, then switch back.

4. Redeploy — breakwater FIRST (new routing + cert), then the rest:
   cd ~/code/breakwater && tugboat deploy
   then: tugboat fleet deploy   (or each service), and 'tugboat fleet docs' for pilot.

5. harbor extension — installed browsers still poll the OLD update_url. The
   extension ID is the signing key (unchanged), so ship ONE update over the OLD
   host whose manifest carries the NEW update_url, and keep the old host serving
   until browsers migrate. Do NOT just flip the host or auto-update breaks.

6. ferry-config — this edited the local clone. The LIVE config is /var/lib/ferry
   on deepwa7er (git-autocommit). Push ferry-config and pull/restore it there.

7. iOS apps (Helm, lagoon) — rebuild and reinstall; their URLs are compiled in.

8. tide theme cookie — its Domain= changed, so old-domain cookies are abandoned;
   the theme simply re-sets on first load. No action, just expect it.

Then re-run this script (dry-run) to confirm 0 remaining occurrences, and
'tugboat fleet status' to review the diffs before committing.
────────────────────────────────────────────────────────────────────────────
CHECKLIST
