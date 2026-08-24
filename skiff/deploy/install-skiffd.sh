#!/usr/bin/env bash
# Build and atomically install skiffd as a systemd user service on the Fedora
# desktop. This is the production path for the single Rust + React stack.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
SHARE_DIR="${HOME}/.local/share/skiffd"
RELEASE_DIR="${SHARE_DIR}/releases"
STATE_DIR="${HOME}/.local/state/skiff"
WRAPPER_DIR="${HOME}/.config/skiff"
UNIT_DIR="${HOME}/.config/systemd/user"

for command in bun cargo curl tailscale systemctl; do
  command -v "${command}" >/dev/null || {
    echo "${command} is required" >&2
    exit 1
  }
done

echo "==> building the client"
(cd "${REPO}/web" && bun install --frozen-lockfile && bun run build && bun run test)

echo "==> building skiffd"
(cd "${REPO}/.." && cargo build --release -p skiff)

echo "==> staging"
mkdir -p "${BIN_DIR}" "${RELEASE_DIR}" "${STATE_DIR}" "${WRAPPER_DIR}" "${UNIT_DIR}"
install -m 755 "${REPO}/../target/release/skiff" "${BIN_DIR}/.skiffd.new"

release="release-$(date -u +%Y%m%dT%H%M%SZ)-$$"
staged_web="${RELEASE_DIR}/${release}"
mkdir "${staged_web}"
cp -R "${REPO}/web/dist/." "${staged_web}/"

install -m 700 "${REPO}/deploy/skiffd.sh" "${WRAPPER_DIR}/skiffd.sh.new"
install -m 644 "${REPO}/deploy/skiffd.service" "${UNIT_DIR}/skiffd.service.new"
install -m 644 "${REPO}/deploy/opencode-serve.service" "${UNIT_DIR}/opencode-serve.service.new"

tailnet_ip="$(tailscale ip -4)"
previous_web="$(readlink "${SHARE_DIR}/current" 2>/dev/null || true)"
previous_binary=false
if [ -f "${BIN_DIR}/skiffd" ]; then
  cp -p "${BIN_DIR}/skiffd" "${BIN_DIR}/.skiffd.previous"
  previous_binary=true
fi
for file in "${WRAPPER_DIR}/skiffd.sh" "${UNIT_DIR}/skiffd.service"; do
  if [ -f "${file}" ]; then
    cp -p "${file}" "${file}.previous"
  fi
done

legacy_active=()
for unit in skiff.service skiff-bridge.service com.deepwa7er.pi-bridge.service; do
  if systemctl --user is-active --quiet "${unit}"; then
    legacy_active+=("${unit}")
  fi
  systemctl --user stop "${unit}" >/dev/null 2>&1 || true
done

echo "==> installing"
mv -f "${BIN_DIR}/.skiffd.new" "${BIN_DIR}/skiffd"
ln -sfn "releases/${release}" "${SHARE_DIR}/current.new"
mv -Tf "${SHARE_DIR}/current.new" "${SHARE_DIR}/current"
mv -f "${WRAPPER_DIR}/skiffd.sh.new" "${WRAPPER_DIR}/skiffd.sh"
mv -f "${UNIT_DIR}/skiffd.service.new" "${UNIT_DIR}/skiffd.service"
mv -f "${UNIT_DIR}/opencode-serve.service.new" "${UNIT_DIR}/opencode-serve.service"

systemctl --user daemon-reload
systemctl --user enable skiffd.service

if [ -x "${HOME}/.opencode/bin/opencode" ]; then
  systemctl --user enable --now opencode-serve.service
else
  systemctl --user disable --now opencode-serve.service >/dev/null 2>&1 || true
  echo "    opencode is absent; its source will be reported unavailable"
fi

systemctl --user restart skiffd.service
loginctl enable-linger "${USER}" >/dev/null 2>&1 || true

for _ in $(seq 1 20); do
  if curl -fsS "http://${tailnet_ip}:8120/healthz" >/dev/null; then
    echo "==> retiring the Rails and Node services"
    for unit in skiff.service skiff-bridge.service com.deepwa7er.pi-bridge.service; do
      systemctl --user disable "${unit}" >/dev/null 2>&1 || true
    done
    rm -f \
      "${UNIT_DIR}/skiff.service" \
      "${UNIT_DIR}/skiff-bridge.service" \
      "${UNIT_DIR}/com.deepwa7er.pi-bridge.service" \
      "${WRAPPER_DIR}/skiff-server.sh" \
      "${WRAPPER_DIR}/skiff-bridge.sh" \
      "${WRAPPER_DIR}/pi-bridge.sh" \
      "${WRAPPER_DIR}/secrets" \
      "${BIN_DIR}/.skiffd.previous" \
      "${WRAPPER_DIR}/skiffd.sh.previous" \
      "${UNIT_DIR}/skiffd.service.previous"
    systemctl --user daemon-reload
    echo "skiffd is running:"
    echo "  https://skiff.intern.deepwa7er.net"
    echo "  http://${tailnet_ip}:8120"
    exit 0
  fi
  sleep 0.5
done

echo "skiffd did not become healthy; last log lines:" >&2
tail -40 "${STATE_DIR}/skiffd.log" >&2 || true
echo "==> rolling back" >&2
systemctl --user stop skiffd.service >/dev/null 2>&1 || true
if ${previous_binary}; then
  mv -f "${BIN_DIR}/.skiffd.previous" "${BIN_DIR}/skiffd"
fi
if [ -n "${previous_web}" ]; then
  ln -sfn "${previous_web}" "${SHARE_DIR}/current.new"
  mv -Tf "${SHARE_DIR}/current.new" "${SHARE_DIR}/current"
fi
if [ -f "${WRAPPER_DIR}/skiffd.sh.previous" ]; then
  mv -f "${WRAPPER_DIR}/skiffd.sh.previous" "${WRAPPER_DIR}/skiffd.sh"
fi
if [ -f "${UNIT_DIR}/skiffd.service.previous" ]; then
  mv -f "${UNIT_DIR}/skiffd.service.previous" "${UNIT_DIR}/skiffd.service"
fi
systemctl --user daemon-reload
if ${previous_binary}; then
  systemctl --user restart skiffd.service >/dev/null 2>&1 || true
fi
for unit in "${legacy_active[@]}"; do
  systemctl --user start "${unit}" >/dev/null 2>&1 || true
done
exit 1
