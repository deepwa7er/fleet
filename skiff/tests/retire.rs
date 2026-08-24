#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    script: PathBuf,
    systemctl: PathBuf,
    docker: PathBuf,
    curl: PathBuf,
    id: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let holder = tempfile::tempdir().unwrap();
        let root = holder.path().join("root");
        let state = holder.path().join("state");
        let bin = holder.path().join("bin");
        for directory in [
            root.join("etc/systemd/system/lighthouse.target.wants"),
            root.join("opt/skiff"),
            root.join("usr/local/bin"),
            root.join("run"),
            root.join("var/lib/tugboat"),
            state.clone(),
            bin.clone(),
        ] {
            fs::create_dir_all(directory).unwrap();
        }

        fs::write(
            root.join("etc/systemd/system/skiff.service"),
            "[Service]\nExecStart=/usr/bin/docker run --rm --name skiff \\\n+  skiff:deploy\n",
        )
        .unwrap();
        symlink(
            "../skiff.service",
            root.join("etc/systemd/system/lighthouse.target.wants/skiff.service"),
        )
        .unwrap();
        for artifact in [
            "bridge-secrets",
            "bridge-secrets.bak-pre-multiharness",
            "master.key",
            "skiff-image.tar",
        ] {
            fs::write(root.join("opt/skiff").join(artifact), "legacy").unwrap();
        }
        fs::write(
            root.join("var/lib/tugboat/skiff.jsonl"),
            "historical deployment\n",
        )
        .unwrap();
        fs::write(
            root.join("usr/local/bin/skiff-resolve-bridge"),
            "#!/bin/sh\nout=\"/run/skiff-bridge.env\"\necho \"SKIFF_BRIDGE_URL=http://host.docker.internal:4120\" > \"$out\"\n",
        )
        .unwrap();
        fs::write(
            root.join("run/skiff-bridge.env"),
            "SKIFF_BRIDGE_PASSWORD=retired\n",
        )
        .unwrap();
        fs::write(state.join("active-skiff.service"), "").unwrap();
        fs::write(state.join("enabled-skiff.service"), "").unwrap();
        fs::write(state.join("container-skiff"), "").unwrap();
        fs::write(state.join("image-skiff-deploy"), "").unwrap();

        let systemctl = bin.join("systemctl");
        let docker = bin.join("docker");
        let curl = bin.join("curl");
        let id = bin.join("id");
        write_executable(&systemctl, FAKE_SYSTEMCTL);
        write_executable(&docker, FAKE_DOCKER);
        write_executable(
            &curl,
            "#!/usr/bin/env bash\nset -euo pipefail\n[[ \"${FAKE_HEALTH:-ok}\" == ok ]] && printf ok\n",
        );
        write_executable(&id, "#!/usr/bin/env bash\nprintf '0\\n'\n");

        Self {
            _root: holder,
            root,
            state,
            script: Path::new(env!("CARGO_MANIFEST_DIR")).join("deploy/retire-vps.sh"),
            systemctl,
            docker,
            curl,
            id,
        }
    }

    fn run(&self, healthy: bool) -> Output {
        Command::new("bash")
            .arg(&self.script)
            .env("SKIFF_RETIRE_ROOT", &self.root)
            .env("SKIFF_SYSTEMCTL", &self.systemctl)
            .env("SKIFF_DOCKER", &self.docker)
            .env("SKIFF_CURL", &self.curl)
            .env("SKIFF_ID", &self.id)
            .env("FAKE_STATE", &self.state)
            .env("FAKE_HEALTH", if healthy { "ok" } else { "down" })
            .output()
            .unwrap()
    }

    fn assert_legacy_present(&self) {
        assert!(self.root.join("etc/systemd/system/skiff.service").exists());
        assert!(
            self.root
                .join("etc/systemd/system/lighthouse.target.wants/skiff.service")
                .is_symlink()
        );
        assert!(self.root.join("opt/skiff").is_dir());
        assert!(
            self.root
                .join("usr/local/bin/skiff-resolve-bridge")
                .exists()
        );
        assert!(self.root.join("run/skiff-bridge.env").exists());
        assert!(self.state.join("active-skiff.service").exists());
        assert!(self.state.join("container-skiff").exists());
        assert!(self.state.join("image-skiff-deploy").exists());
    }
}

#[test]
fn a_healthy_cutover_retires_only_the_known_vps_state_and_is_idempotent() {
    let fixture = Fixture::new();
    for attempt in 1..=2 {
        let output = fixture.run(true);
        assert!(
            output.status.success(),
            "attempt {attempt} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert!(
        !fixture
            .root
            .join("etc/systemd/system/skiff.service")
            .exists()
    );
    assert!(
        !fixture
            .root
            .join("etc/systemd/system/lighthouse.target.wants/skiff.service")
            .exists()
    );
    assert!(!fixture.root.join("opt/skiff").exists());
    assert!(
        !fixture
            .root
            .join("usr/local/bin/skiff-resolve-bridge")
            .exists()
    );
    assert!(!fixture.root.join("run/skiff-bridge.env").exists());
    assert!(!fixture.state.join("active-skiff.service").exists());
    assert!(!fixture.state.join("enabled-skiff.service").exists());
    assert!(!fixture.state.join("container-skiff").exists());
    assert!(!fixture.state.join("image-skiff-deploy").exists());
    assert_eq!(
        fs::read_to_string(fixture.root.join("var/lib/tugboat/skiff.jsonl")).unwrap(),
        "historical deployment\n"
    );
}

#[test]
fn an_unhealthy_replacement_preserves_every_rollback_artifact() {
    let fixture = Fixture::new();
    let output = fixture.run(false);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("refusing retirement"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.assert_legacy_present();
}

#[test]
fn an_unknown_vps_artifact_is_never_deleted() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("opt/skiff/authored.sqlite3"), "keep me").unwrap();
    let output = fixture.run(true);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unexpected Skiff VPS artifact"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.assert_legacy_present();
    assert_eq!(
        fs::read_to_string(fixture.root.join("opt/skiff/authored.sqlite3")).unwrap(),
        "keep me"
    );
}

#[test]
fn a_different_unit_with_the_same_name_is_never_removed() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("etc/systemd/system/skiff.service"),
        "[Service]\nExecStart=/usr/local/bin/a-different-skiff\n",
    )
    .unwrap();
    let output = fixture.run(true);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not the retired Rails Skiff unit"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.assert_legacy_present();
    assert_eq!(
        fs::read_to_string(fixture.root.join("etc/systemd/system/skiff.service")).unwrap(),
        "[Service]\nExecStart=/usr/local/bin/a-different-skiff\n"
    );
}

#[test]
fn a_different_bridge_resolver_is_never_removed() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("usr/local/bin/skiff-resolve-bridge"),
        "#!/bin/sh\nexec /usr/local/bin/something-new\n",
    )
    .unwrap();
    let output = fixture.run(true);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not the retired Skiff resolver"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.assert_legacy_present();
    assert_eq!(
        fs::read_to_string(fixture.root.join("usr/local/bin/skiff-resolve-bridge")).unwrap(),
        "#!/bin/sh\nexec /usr/local/bin/something-new\n"
    );
}

#[test]
fn a_different_lighthouse_enrollment_is_never_removed() {
    let fixture = Fixture::new();
    let link = fixture
        .root
        .join("etc/systemd/system/lighthouse.target.wants/skiff.service");
    fs::remove_file(&link).unwrap();
    symlink("../another.service", &link).unwrap();

    let output = fixture.run(true);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not enroll the retired Skiff unit"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.assert_legacy_present();
    assert_eq!(
        fs::read_link(link).unwrap(),
        Path::new("../another.service")
    );
}

#[test]
fn a_different_container_with_the_same_name_is_never_removed() {
    let fixture = Fixture::new();
    fs::write(fixture.state.join("foreign-container-skiff"), "").unwrap();

    let output = fixture.run(true);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not the retired Rails Skiff"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.assert_legacy_present();
}

#[test]
fn a_different_image_with_the_same_tag_is_never_removed() {
    let fixture = Fixture::new();
    fs::write(fixture.state.join("foreign-image-skiff-deploy"), "").unwrap();

    let output = fixture.run(true);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not the retired Rails Skiff"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.assert_legacy_present();
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

const FAKE_SYSTEMCTL: &str = r#"#!/usr/bin/env bash
set -euo pipefail
state="${FAKE_STATE:?}"
verb="${1:?}"
shift
case "${verb}" in
  is-active)
    [[ "${1:-}" == --quiet ]] && shift
    [[ -f "${state}/active-${1:?}" ]]
    ;;
  disable)
    [[ "${1:-}" == --now ]] && shift
    rm -f "${state}/active-${1:?}" "${state}/enabled-${1:?}"
    ;;
  daemon-reload | reset-failed)
    ;;
  *)
    echo "unexpected systemctl verb: ${verb}" >&2
    exit 64
    ;;
esac
"#;

const FAKE_DOCKER: &str = r#"#!/usr/bin/env bash
set -euo pipefail
state="${FAKE_STATE:?}"
family="${1:?}"
verb="${2:?}"
shift 2
case "${family}:${verb}" in
  container:inspect)
    if [[ "${1:-}" == --format ]]; then
      shift 2
      [[ "${1:?}" == skiff && -f "${state}/container-skiff" ]]
      if [[ -f "${state}/foreign-container-skiff" ]]; then
        printf 'something:new|["/foreign"]|["serve"]|/foreign\n'
      else
        printf 'skiff:deploy|["/rails/bin/docker-entrypoint"]|["./bin/rails","server"]|/rails\n'
      fi
    else
      [[ "${1:?}" == skiff && -f "${state}/container-skiff" ]]
    fi
    ;;
  image:inspect)
    if [[ "${1:-}" == --format ]]; then
      shift 2
      [[ "${1:?}" == skiff:deploy && -f "${state}/image-skiff-deploy" ]]
      if [[ -f "${state}/foreign-image-skiff-deploy" ]]; then
        printf '["/foreign"]|["serve"]|/foreign\n'
      else
        printf '["/rails/bin/docker-entrypoint"]|["./bin/rails","server"]|/rails\n'
      fi
    else
      [[ "${1:?}" == skiff:deploy && -f "${state}/image-skiff-deploy" ]]
    fi
    ;;
  rm:-f)
    [[ "${1:?}" == skiff ]]
    rm -f "${state}/container-skiff"
    ;;
  image:rm)
    [[ "${1:?}" == skiff:deploy ]]
    rm -f "${state}/image-skiff-deploy"
    ;;
  *)
    echo "unexpected docker command: ${family} ${verb} $*" >&2
    exit 64
    ;;
esac
"#;
