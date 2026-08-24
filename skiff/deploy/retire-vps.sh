#!/usr/bin/env bash
# Retire the obsolete Rails Skiff from the VPS after the Rust desktop service
# owns https://skiff.intern.deepwa7er.net.
#
# This is deliberately a separate, post-cutover operation. The desktop
# installer can roll back until its health check commits, and Breakwater can
# roll back independently; deleting the old VPS artifacts before both have
# succeeded would turn either rollback into a reconstruction exercise.
set -euo pipefail

ROOT="${SKIFF_RETIRE_ROOT:-}"
SYSTEMCTL="${SKIFF_SYSTEMCTL:-/usr/bin/systemctl}"
DOCKER="${SKIFF_DOCKER:-/usr/bin/docker}"
CURL="${SKIFF_CURL:-/usr/bin/curl}"
ID="${SKIFF_ID:-/usr/bin/id}"

if [[ -n "${ROOT}" ]]; then
  ROOT="${ROOT%/}"
  [[ "${ROOT}" == /* && "${ROOT}" != / ]] || {
    echo "SKIFF_RETIRE_ROOT must be an absolute path other than /" >&2
    exit 64
  }
  # A rooted run is the integration-test/staging seam. Requiring every
  # stateful command to be injected prevents it from accidentally controlling
  # the real host while inspecting a staged filesystem.
  for variable in SKIFF_SYSTEMCTL SKIFF_DOCKER SKIFF_CURL SKIFF_ID; do
    [[ -n "${!variable:-}" ]] || {
      echo "${variable} is required when SKIFF_RETIRE_ROOT is set" >&2
      exit 64
    }
  done
fi

for command in "${SYSTEMCTL}" "${DOCKER}" "${CURL}" "${ID}"; do
  [[ -x "${command}" ]] || {
    echo "required command is not executable: ${command}" >&2
    exit 69
  }
done

[[ "$("${ID}" -u)" == 0 ]] || {
  echo "retiring the VPS service requires root" >&2
  exit 77
}

UNIT=skiff.service
CONTAINER=skiff
IMAGE=skiff:deploy
UNIT_FILE="${ROOT}/etc/systemd/system/${UNIT}"
LIGHTHOUSE_LINK="${ROOT}/etc/systemd/system/lighthouse.target.wants/${UNIT}"
APP_DIR="${ROOT}/opt/skiff"
BRIDGE_RESOLVER="${ROOT}/usr/local/bin/skiff-resolve-bridge"
BRIDGE_ENV="${ROOT}/run/skiff-bridge.env"
CUTOVER_URL=https://skiff.intern.deepwa7er.net/healthz

# Refuse to remove a unit that merely happens to share Skiff's name. These two
# lines identify the retired Rails container contract, not the Rust service on
# the desktop.
if [[ -e "${UNIT_FILE}" || -L "${UNIT_FILE}" ]]; then
  [[ -f "${UNIT_FILE}" && ! -L "${UNIT_FILE}" ]] || {
    echo "refusing non-regular unit file: ${UNIT_FILE}" >&2
    exit 65
  }
  grep -Fq 'ExecStart=/usr/bin/docker run --rm --name skiff' "${UNIT_FILE}" &&
    grep -Fq 'skiff:deploy' "${UNIT_FILE}" || {
      echo "${UNIT_FILE} is not the retired Rails Skiff unit; refusing removal" >&2
      exit 65
    }
fi

if [[ -e "${LIGHTHOUSE_LINK}" || -L "${LIGHTHOUSE_LINK}" ]]; then
  [[ -L "${LIGHTHOUSE_LINK}" ]] || {
    echo "refusing non-symlink Lighthouse enrollment: ${LIGHTHOUSE_LINK}" >&2
    exit 65
  }
  lighthouse_target="$(readlink "${LIGHTHOUSE_LINK}")"
  [[ "${lighthouse_target}" == /etc/systemd/system/skiff.service ||
    "${lighthouse_target}" == ../skiff.service ]] || {
    echo "${LIGHTHOUSE_LINK} does not enroll the retired Skiff unit; refusing removal" >&2
    exit 65
  }
fi

if [[ ! -e "${UNIT_FILE}" && ! -L "${UNIT_FILE}" ]] &&
  "${SYSTEMCTL}" is-active --quiet "${UNIT}"; then
  echo "${UNIT} is active without the retired unit file; refusing removal" >&2
  exit 65
fi

if [[ -e "${BRIDGE_RESOLVER}" || -L "${BRIDGE_RESOLVER}" ]]; then
  [[ -f "${BRIDGE_RESOLVER}" && ! -L "${BRIDGE_RESOLVER}" ]] || {
    echo "refusing non-regular bridge resolver: ${BRIDGE_RESOLVER}" >&2
    exit 65
  }
  grep -Fq 'out="/run/skiff-bridge.env"' "${BRIDGE_RESOLVER}" &&
    grep -Fq 'SKIFF_BRIDGE_URL=http://host.docker.internal:4120' "${BRIDGE_RESOLVER}" || {
      echo "${BRIDGE_RESOLVER} is not the retired Skiff resolver; refusing removal" >&2
      exit 65
    }
fi

rails_contract='["/rails/bin/docker-entrypoint"]|["./bin/rails","server"]|/rails'
if "${DOCKER}" container inspect "${CONTAINER}" >/dev/null 2>&1; then
  container_contract="$(
    "${DOCKER}" container inspect \
      --format '{{.Config.Image}}|{{json .Config.Entrypoint}}|{{json .Config.Cmd}}|{{.Config.WorkingDir}}' \
      "${CONTAINER}"
  )"
  [[ "${container_contract}" == "${IMAGE}|${rails_contract}" ]] || {
    echo "container ${CONTAINER} is not the retired Rails Skiff; refusing removal" >&2
    exit 65
  }
fi
if "${DOCKER}" image inspect "${IMAGE}" >/dev/null 2>&1; then
  image_contract="$(
    "${DOCKER}" image inspect \
      --format '{{json .Config.Entrypoint}}|{{json .Config.Cmd}}|{{.Config.WorkingDir}}' \
      "${IMAGE}"
  )"
  [[ "${image_contract}" == "${rails_contract}" ]] || {
    echo "image ${IMAGE} is not the retired Rails Skiff; refusing removal" >&2
    exit 65
  }
fi

# The Rails deployment had no authored application state on the VPS. Refuse
# surprises rather than recursively deleting a directory that has acquired a
# new purpose since this script was written.
allowed_artifacts=(
  bridge-secrets
  bridge-secrets.bak-pre-multiharness
  master.key
  skiff-image.tar
)
if [[ -e "${APP_DIR}" || -L "${APP_DIR}" ]]; then
  [[ -d "${APP_DIR}" && ! -L "${APP_DIR}" ]] || {
    echo "refusing non-directory application path: ${APP_DIR}" >&2
    exit 65
  }
  while IFS= read -r -d '' artifact; do
    name="$(basename "${artifact}")"
    known=false
    for allowed in "${allowed_artifacts[@]}"; do
      [[ "${name}" == "${allowed}" ]] && known=true
    done
    if ! ${known} || [[ ! -f "${artifact}" || -L "${artifact}" ]]; then
      echo "unexpected Skiff VPS artifact; refusing removal: ${artifact}" >&2
      exit 65
    fi
  done < <(find "${APP_DIR}" -mindepth 1 -maxdepth 1 -print0)
fi

# This endpoint does not exist in Rails. An exact body of "ok" proves that
# Breakwater has already flipped the canonical hostname to skiffd before the
# last rollback copy is destroyed.
health="$("${CURL}" -fsS "${CUTOVER_URL}")" || {
  echo "Rust Skiff is not healthy at ${CUTOVER_URL}; refusing retirement" >&2
  exit 1
}
[[ "${health}" == ok ]] || {
  echo "unexpected health response from ${CUTOVER_URL}; refusing retirement" >&2
  exit 1
}

echo "==> stopping and disabling ${UNIT}"
if [[ -e "${UNIT_FILE}" ]] || "${SYSTEMCTL}" is-active --quiet "${UNIT}"; then
  "${SYSTEMCTL}" disable --now "${UNIT}"
fi

echo "==> removing Lighthouse enrollment and unit"
rm -f -- "${LIGHTHOUSE_LINK}"
rm -f -- "${UNIT_FILE}"
rm -f -- "${BRIDGE_RESOLVER}" "${BRIDGE_ENV}"
"${SYSTEMCTL}" daemon-reload
"${SYSTEMCTL}" reset-failed "${UNIT}" >/dev/null 2>&1 || true

echo "==> removing the exact Rails container and image tag"
if "${DOCKER}" container inspect "${CONTAINER}" >/dev/null 2>&1; then
  "${DOCKER}" rm -f "${CONTAINER}" >/dev/null
fi
if "${DOCKER}" image inspect "${IMAGE}" >/dev/null 2>&1; then
  "${DOCKER}" image rm "${IMAGE}" >/dev/null
fi

echo "==> removing the known Rails artifacts"
if [[ -d "${APP_DIR}" ]]; then
  for name in "${allowed_artifacts[@]}"; do
    rm -f -- "${APP_DIR}/${name}"
  done
  rmdir -- "${APP_DIR}"
fi

"${SYSTEMCTL}" is-active --quiet "${UNIT}" && {
  echo "${UNIT} is still active" >&2
  exit 1
}
[[ ! -e "${UNIT_FILE}" && ! -L "${UNIT_FILE}" ]]
[[ ! -e "${LIGHTHOUSE_LINK}" && ! -L "${LIGHTHOUSE_LINK}" ]]
[[ ! -e "${APP_DIR}" && ! -L "${APP_DIR}" ]]
[[ ! -e "${BRIDGE_RESOLVER}" && ! -L "${BRIDGE_RESOLVER}" ]]
[[ ! -e "${BRIDGE_ENV}" && ! -L "${BRIDGE_ENV}" ]]
! "${DOCKER}" container inspect "${CONTAINER}" >/dev/null 2>&1
! "${DOCKER}" image inspect "${IMAGE}" >/dev/null 2>&1

echo "Rails Skiff retired from the VPS. Tugboat's deployment ledger was preserved."
