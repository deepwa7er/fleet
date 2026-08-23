#!/usr/bin/env bash
#
# Enables Wake-on-LAN (magic packet) on the desktop's wired NIC so the
# desktop can be powered on remotely — see deploy/wake-desktop, the sender.
#
# Run ONCE on the desktop:
#     deploy/enable-wol.sh          # re-execs under sudo if not root
#
# BIOS, done once by hand (this script cannot): enable "Wake on LAN" /
# "Power On By PCI-E" in the BIOS, and disable ErP / Deep Sleep (S5) if the
# option exists — ErP cuts standby power and WoL cannot work.
#
# Also done once, and just as load-bearing: plymouth must be disabled on the
# kernel command line. A WoL wake happens with the monitor off, and plymouthd
# is a DRM client — with no connected CRTC its socket call blocks, which hangs
# plymouth-read-write.service (Before=sysinit.target, and Type=oneshot, for
# which systemd applies no start timeout) before NetworkManager ever runs. The
# machine then powers on but never reaches the network, so it looks alive and
# is unreachable. Worse, because NM never ran, the arm below is never
# re-applied, so the *next* magic packet is ignored too and only the physical
# power button gets you back. Apply it with:
#     grubby --update-kernel=ALL --args="plymouth.enable=0"
#
# What this does:
#   - Sets the NetworkManager profile's 802-3-ethernet.wake-on-lan property
#     to "magic". This persists across reboots; NM re-applies it at every
#     connection-up, so the driver state survives NIC resets.
#   - Re-activates the connection so the setting takes effect immediately
#     (a brief network blip).
#   - Verifies with ethtool that the NIC now reports Wake-on: g.
#   - Asserts plymouth is disabled on every configured boot entry, so a
#     headless wake can actually finish booting. A green run means remote
#     power-on works end to end, not merely that the NIC is armed.
#
# Idempotent: safe to re-run. Requires NetworkManager, ethtool and grubby.
set -euo pipefail

IFACE=enp9s0
EXPECTED_MAC=fc:9d:05:05:cb:84
PLYMOUTH_ARG=plymouth.enable=0

if [ "$(id -u)" -ne 0 ]; then
    exec sudo "$0" "$@"
fi

# The NetworkManager profile that owns the wired NIC.
CONN="$(nmcli -t -f NAME,DEVICE con show | awk -F: -v dev="$IFACE" '$2 == dev {print $1; exit}')"
if [ -z "${CONN}" ]; then
    echo "no NetworkManager profile for ${IFACE}; is it managed by NM?" >&2
    exit 1
fi

# Guard against interface renames / NIC swaps: wake-desktop broadcasts this
# MAC, so a mismatch would silently break remote power-on.
ACTUAL_MAC="$(cat "/sys/class/net/${IFACE}/address")"
if [ "${ACTUAL_MAC}" != "${EXPECTED_MAC}" ]; then
    echo "${IFACE} MAC is ${ACTUAL_MAC}, expected ${EXPECTED_MAC} — update deploy/wake-desktop" >&2
    exit 1
fi

nmcli con modify "${CONN}" 802-3-ethernet.wake-on-lan magic
nmcli con up "${CONN}" >/dev/null

# Anchored so it matches "Wake-on: g" but not the "Supports Wake-on: pumbg"
# line ethtool prints just above it.
WAKE="$(ethtool "${IFACE}" | awk -F: '/^[[:space:]]*Wake-on/ {gsub(/[[:space:]]/, "", $2); print $2}')"
if [ "${WAKE}" != "g" ]; then
    echo "warning: ${IFACE} reports Wake-on: ${WAKE} (expected g) — the NIC or its driver may not support WoL." >&2
    exit 1
fi

# An armed NIC is only half of remote power-on: the machine also has to finish
# booting with no display attached. See the plymouth note in the header.
if ! command -v grubby >/dev/null 2>&1; then
    echo "grubby not found — cannot verify ${PLYMOUTH_ARG} on the boot entries." >&2
    echo "Confirm by hand that every boot entry disables plymouth, or a headless wake will hang." >&2
    exit 1
fi

# Assign first so a grubby failure trips `set -e` here, rather than silently
# feeding an empty list to the loop and reporting a false "all guarded".
GRUBBY_INFO="$(grubby --info=ALL)"

# Boot entries are what govern the *next* (headless) boot, so check every one
# of them — including rescue, which is exactly the entry you fall back to when
# something has already gone wrong.
UNGUARDED=""
INDEX="?"
while IFS= read -r line; do
    case "${line}" in
        index=*) INDEX="${line#index=}" ;;
        args=*)
            case "${line}" in
                *"${PLYMOUTH_ARG}"*) ;;
                *) UNGUARDED="${UNGUARDED}${UNGUARDED:+ }${INDEX}" ;;
            esac
            ;;
    esac
done <<< "${GRUBBY_INFO}"

if [ -n "${UNGUARDED}" ]; then
    echo "boot entries missing ${PLYMOUTH_ARG}: ${UNGUARDED}" >&2
    echo "A magic packet would power the desktop on, then it would hang before the network." >&2
    echo "Fix with: grubby --update-kernel=ALL --args=\"${PLYMOUTH_ARG}\"" >&2
    exit 1
fi

echo "ok: ${IFACE} (${CONN}) reports Wake-on: g — WoL is live."
echo "ok: every boot entry sets ${PLYMOUTH_ARG} — a headless wake can reach the network."

# The running kernel is not what the next wake boots, so a missing arg here is
# a note rather than a failure — the entries above already guarantee the wake.
if ! grep -qF -- "${PLYMOUTH_ARG}" /proc/cmdline; then
    echo "note: the running kernel predates the change; it takes effect on the next boot."
fi

echo "Test it: power the desktop off with the monitor off, then: ssh laptop wake-desktop"
