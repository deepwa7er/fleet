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
# What this does:
#   - Sets the NetworkManager profile's 802-3-ethernet.wake-on-lan property
#     to "magic". This persists across reboots; NM re-applies it at every
#     connection-up, so the driver state survives NIC resets.
#   - Re-activates the connection so the setting takes effect immediately
#     (a brief network blip).
#   - Verifies with ethtool that the NIC now reports Wake-on: g.
#
# Idempotent: safe to re-run. Requires NetworkManager + ethtool.
set -euo pipefail

IFACE=enp9s0
EXPECTED_MAC=fc:9d:05:05:cb:84

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

WAKE="$(ethtool "${IFACE}" | awk -F: '/^[[:space:]]*Wake-on/ {gsub(/[[:space:]]/, "", $2); print $2}')"
if [ "${WAKE}" = "g" ]; then
    echo "ok: ${IFACE} (${CONN}) reports Wake-on: g — WoL is live."
    echo "Test it: power the desktop off, then from any machine: ssh laptop wake-desktop"
else
    echo "warning: ${IFACE} reports Wake-on: ${WAKE} (expected g) — the NIC or its driver may not support WoL." >&2
    exit 1
fi
