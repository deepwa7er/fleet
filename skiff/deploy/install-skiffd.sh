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
backup_dir="$(mktemp -d "${STATE_DIR}/install-backup.XXXXXX")"

backup_artifact() {
  local source="$1"
  local name="$2"
  if [ -e "${source}" ] || [ -L "${source}" ]; then
    cp -a -- "${source}" "${backup_dir}/${name}"
  fi
}

restore_artifact() {
  local name="$1"
  local target="$2"
  rm -f -- "${target}"
  if [ -e "${backup_dir}/${name}" ] || [ -L "${backup_dir}/${name}" ]; then
    cp -a -- "${backup_dir}/${name}" "${target}"
  fi
}

backup_artifact "${BIN_DIR}/skiffd" binary
backup_artifact "${SHARE_DIR}/current" current
backup_artifact "${WRAPPER_DIR}/skiffd.sh" wrapper
backup_artifact "${UNIT_DIR}/skiffd.service" skiffd-unit
backup_artifact "${UNIT_DIR}/opencode-serve.service" opencode-unit
backup_artifact "${UNIT_DIR}/skiff.service" legacy-skiff-unit
backup_artifact "${UNIT_DIR}/skiff-bridge.service" legacy-bridge-unit
backup_artifact "${UNIT_DIR}/com.deepwa7er.pi-bridge.service" legacy-pi-unit
backup_artifact "${WRAPPER_DIR}/skiff-server.sh" legacy-skiff-wrapper
backup_artifact "${WRAPPER_DIR}/skiff-bridge.sh" legacy-bridge-wrapper
backup_artifact "${WRAPPER_DIR}/pi-bridge.sh" legacy-pi-wrapper
backup_artifact "${WRAPPER_DIR}/secrets" legacy-secrets

skiffd_was_enabled=false
skiffd_was_active=false
opencode_was_enabled=false
opencode_was_active=false
systemctl --user is-enabled --quiet skiffd.service && skiffd_was_enabled=true
systemctl --user is-active --quiet skiffd.service && skiffd_was_active=true
systemctl --user is-enabled --quiet opencode-serve.service && opencode_was_enabled=true
systemctl --user is-active --quiet opencode-serve.service && opencode_was_active=true

legacy_units=(skiff.service skiff-bridge.service com.deepwa7er.pi-bridge.service)
legacy_active=()
legacy_enabled=()
for unit in "${legacy_units[@]}"; do
  if systemctl --user is-active --quiet "${unit}"; then
    legacy_active+=("${unit}")
  fi
  if systemctl --user is-enabled --quiet "${unit}"; then
    legacy_enabled+=("${unit}")
  fi
done

rollback() {
  echo "==> rolling back" >&2
  local failed=false
  systemctl --user stop skiffd.service opencode-serve.service >/dev/null 2>&1 || true
  systemctl --user disable skiffd.service opencode-serve.service >/dev/null 2>&1 || true

  restore_artifact binary "${BIN_DIR}/skiffd" || failed=true
  restore_artifact current "${SHARE_DIR}/current" || failed=true
  restore_artifact wrapper "${WRAPPER_DIR}/skiffd.sh" || failed=true
  restore_artifact skiffd-unit "${UNIT_DIR}/skiffd.service" || failed=true
  restore_artifact opencode-unit "${UNIT_DIR}/opencode-serve.service" || failed=true
  restore_artifact legacy-skiff-unit "${UNIT_DIR}/skiff.service" || failed=true
  restore_artifact legacy-bridge-unit "${UNIT_DIR}/skiff-bridge.service" || failed=true
  restore_artifact legacy-pi-unit "${UNIT_DIR}/com.deepwa7er.pi-bridge.service" || failed=true
  restore_artifact legacy-skiff-wrapper "${WRAPPER_DIR}/skiff-server.sh" || failed=true
  restore_artifact legacy-bridge-wrapper "${WRAPPER_DIR}/skiff-bridge.sh" || failed=true
  restore_artifact legacy-pi-wrapper "${WRAPPER_DIR}/pi-bridge.sh" || failed=true
  restore_artifact legacy-secrets "${WRAPPER_DIR}/secrets" || failed=true
  rm -rf -- "${staged_web}"

  systemctl --user daemon-reload || failed=true
  if ${skiffd_was_enabled}; then
    systemctl --user enable skiffd.service >/dev/null 2>&1 || failed=true
  fi
  if ${opencode_was_enabled}; then
    systemctl --user enable opencode-serve.service >/dev/null 2>&1 || failed=true
  fi
  for unit in "${legacy_enabled[@]}"; do
    systemctl --user enable "${unit}" >/dev/null 2>&1 || failed=true
  done
  if ${skiffd_was_active}; then
    systemctl --user restart skiffd.service >/dev/null 2>&1 || failed=true
  fi
  if ${opencode_was_active}; then
    systemctl --user restart opencode-serve.service >/dev/null 2>&1 || failed=true
  fi
  for unit in "${legacy_active[@]}"; do
    systemctl --user start "${unit}" >/dev/null 2>&1 || failed=true
  done

  if ${failed}; then
    echo "rollback was incomplete; recovery artifacts remain in ${backup_dir}" >&2
    return 1
  fi
  rm -rf -- "${backup_dir}"
  echo "previous services and artifacts restored" >&2
}

committed=false
on_exit() {
  local status=$?
  trap - EXIT HUP INT TERM
  if ! ${committed}; then
    set +e
    rollback
    if [ $? -ne 0 ] && [ "${status}" -eq 0 ]; then
      status=1
    fi
  fi
  exit "${status}"
}
trap on_exit EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

for unit in "${legacy_units[@]}"; do
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
    for unit in "${legacy_enabled[@]}"; do
      systemctl --user disable "${unit}"
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
    committed=true
    trap - EXIT HUP INT TERM
    rm -rf -- "${backup_dir}" ||
      echo "warning: could not remove install backup ${backup_dir}" >&2
    echo "skiffd is running:"
    echo "  https://skiff.intern.deepwa7er.net"
    echo "  http://${tailnet_ip}:8120"
    exit 0
  fi
  sleep 0.5
done

echo "skiffd did not become healthy; last log lines:" >&2
tail -40 "${STATE_DIR}/skiffd.log" >&2 || true
exit 1
