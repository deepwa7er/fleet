#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    installer: PathBuf,
    path: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("fleet");
        let skiff = repo.join("skiff");
        let deploy = skiff.join("deploy");
        let home = root.path().join("home");
        let state = root.path().join("systemd-state");
        let bin = root.path().join("fake-bin");
        for dir in [
            &deploy,
            &skiff.join("web/dist"),
            &repo.join("target/release"),
            &home.join(".config/skiff"),
            &home.join(".config/systemd/user"),
            &state,
            &bin,
        ] {
            fs::create_dir_all(dir).unwrap();
        }

        fs::write(
            deploy.join("install-skiffd.sh"),
            include_str!("../deploy/install-skiffd.sh"),
        )
        .unwrap();
        fs::write(
            deploy.join("skiffd.sh"),
            include_str!("../deploy/skiffd.sh"),
        )
        .unwrap();
        fs::write(
            deploy.join("skiffd.service"),
            include_str!("../deploy/skiffd.service"),
        )
        .unwrap();
        fs::write(
            deploy.join("opencode-serve.service"),
            include_str!("../deploy/opencode-serve.service"),
        )
        .unwrap();
        fs::write(skiff.join("web/dist/index.html"), "new web").unwrap();
        write_executable(&repo.join("target/release/skiff"), "new binary");

        for (path, contents) in [
            (".config/systemd/user/skiff.service", "old skiff unit"),
            (
                ".config/systemd/user/skiff-bridge.service",
                "old bridge unit",
            ),
            (
                ".config/systemd/user/com.deepwa7er.pi-bridge.service",
                "old pi unit",
            ),
            (
                ".config/systemd/user/opencode-serve.service",
                "old opencode unit",
            ),
            (".config/skiff/skiff-server.sh", "old skiff wrapper"),
            (".config/skiff/skiff-bridge.sh", "old bridge wrapper"),
            (".config/skiff/pi-bridge.sh", "old pi wrapper"),
            (".config/skiff/secrets", "old secret"),
        ] {
            fs::write(home.join(path), contents).unwrap();
        }
        for unit in [
            "skiff.service",
            "skiff-bridge.service",
            "com.deepwa7er.pi-bridge.service",
            "opencode-serve.service",
        ] {
            fs::write(state.join(format!("active-{unit}")), "").unwrap();
            fs::write(state.join(format!("enabled-{unit}")), "").unwrap();
        }

        for command in ["bun", "cargo", "loginctl"] {
            write_executable(&bin.join(command), "#!/usr/bin/env bash\nexit 0\n");
        }
        write_executable(
            &bin.join("tailscale"),
            "#!/usr/bin/env bash\n[ \"${1:-}\" = ip ] && printf '100.64.0.8\\n'\n",
        );
        write_executable(
            &bin.join("curl"),
            "#!/usr/bin/env bash\n[ \"${FAKE_CURL_OK:-false}\" = true ]\n",
        );
        write_executable(&bin.join("sleep"), "#!/usr/bin/env bash\nexit 0\n");
        // The production target is Fedora, whose GNU mv provides -T. Adapt
        // only that flag so the transaction can be exercised on the Mac too.
        write_executable(
            &bin.join("mv"),
            r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == -*T* ]]; then
  options="${1//T/}"
  source="${2:?}"
  target="${3:?}"
  rm -f -- "${target}"
  exec /bin/mv "${options}" "${source}" "${target}"
fi
exec /bin/mv "$@"
"#,
        );
        write_executable(&bin.join("systemctl"), FAKE_SYSTEMCTL);

        let path = format!("{}:/usr/bin:/bin", bin.display());
        Self {
            _root: root,
            home,
            state,
            installer: deploy.join("install-skiffd.sh"),
            path,
        }
    }

    fn run(&self, curl_ok: bool, fail_enable: bool) -> Output {
        Command::new("bash")
            .arg(&self.installer)
            .env("HOME", &self.home)
            .env("USER", "tester")
            .env("PATH", &self.path)
            .env("FAKE_SYSTEMD_STATE", &self.state)
            .env("FAKE_CURL_OK", if curl_ok { "true" } else { "false" })
            .env(
                "FAKE_FAIL_ENABLE",
                if fail_enable { "skiffd.service" } else { "" },
            )
            .output()
            .unwrap()
    }
}

#[test]
fn every_failure_after_cutover_begins_restores_the_legacy_install() {
    for fail_enable in [true, false] {
        let fixture = Fixture::new();
        let output = fixture.run(false, fail_enable);
        assert!(!output.status.success(), "installer unexpectedly succeeded");
        assert_legacy_restored(&fixture);
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("previous services and artifacts restored"),
            "stderr was: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn a_healthy_install_commits_the_new_service_and_retires_legacy_state() {
    let fixture = Fixture::new();
    let output = fixture.run(true, false);
    assert!(
        output.status.success(),
        "installer failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join(".local/bin/skiffd")).unwrap(),
        "new binary"
    );
    assert!(
        fixture
            .home
            .join(".local/share/skiffd/current")
            .is_symlink()
    );
    assert!(fixture.state.join("active-skiffd.service").exists());
    assert!(fixture.state.join("enabled-skiffd.service").exists());
    for path in [
        ".config/systemd/user/skiff.service",
        ".config/systemd/user/skiff-bridge.service",
        ".config/systemd/user/com.deepwa7er.pi-bridge.service",
        ".config/skiff/skiff-server.sh",
        ".config/skiff/skiff-bridge.sh",
        ".config/skiff/pi-bridge.sh",
        ".config/skiff/secrets",
    ] {
        assert!(
            !fixture.home.join(path).exists(),
            "legacy artifact remained: {path}"
        );
    }
    for unit in [
        "skiff.service",
        "skiff-bridge.service",
        "com.deepwa7er.pi-bridge.service",
    ] {
        assert!(!fixture.state.join(format!("active-{unit}")).exists());
        assert!(!fixture.state.join(format!("enabled-{unit}")).exists());
    }
}

fn assert_legacy_restored(fixture: &Fixture) {
    for (path, contents) in [
        (".config/systemd/user/skiff.service", "old skiff unit"),
        (
            ".config/systemd/user/skiff-bridge.service",
            "old bridge unit",
        ),
        (
            ".config/systemd/user/com.deepwa7er.pi-bridge.service",
            "old pi unit",
        ),
        (
            ".config/systemd/user/opencode-serve.service",
            "old opencode unit",
        ),
        (".config/skiff/skiff-server.sh", "old skiff wrapper"),
        (".config/skiff/skiff-bridge.sh", "old bridge wrapper"),
        (".config/skiff/pi-bridge.sh", "old pi wrapper"),
        (".config/skiff/secrets", "old secret"),
    ] {
        assert_eq!(
            fs::read_to_string(fixture.home.join(path)).unwrap(),
            contents
        );
    }
    for unit in [
        "skiff.service",
        "skiff-bridge.service",
        "com.deepwa7er.pi-bridge.service",
        "opencode-serve.service",
    ] {
        assert!(fixture.state.join(format!("active-{unit}")).exists());
        assert!(fixture.state.join(format!("enabled-{unit}")).exists());
    }
    for path in [
        ".local/bin/skiffd",
        ".local/share/skiffd/current",
        ".config/skiff/skiffd.sh",
        ".config/systemd/user/skiffd.service",
    ] {
        assert!(
            !fixture.home.join(path).exists(),
            "new artifact survived rollback: {path}"
        );
    }
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

const FAKE_SYSTEMCTL: &str = r#"#!/usr/bin/env bash
set -euo pipefail
state="${FAKE_SYSTEMD_STATE:?}"
if [ "${1:-}" = "--user" ]; then shift; fi
verb="${1:?}"
shift
case "${verb}" in
  is-active)
    [ "${1:-}" = "--quiet" ] && shift
    [ -f "${state}/active-${1:?}" ]
    ;;
  is-enabled)
    [ "${1:-}" = "--quiet" ] && shift
    [ -f "${state}/enabled-${1:?}" ]
    ;;
  daemon-reload)
    ;;
  stop|start|restart)
    for unit in "$@"; do
      [[ "${unit}" = -* ]] && continue
      if [ "${verb}" = stop ]; then
        rm -f "${state}/active-${unit}"
      else
        : > "${state}/active-${unit}"
      fi
    done
    ;;
  enable|disable)
    now=false
    for arg in "$@"; do
      if [ "${arg}" = "--now" ]; then
        now=true
        continue
      fi
      [[ "${arg}" = -* ]] && continue
      if [ "${verb}" = enable ] && [ "${FAKE_FAIL_ENABLE:-}" = "${arg}" ]; then
        exit 42
      fi
      if [ "${verb}" = enable ]; then
        : > "${state}/enabled-${arg}"
        if ${now}; then : > "${state}/active-${arg}"; fi
      else
        rm -f "${state}/enabled-${arg}"
        if ${now}; then rm -f "${state}/active-${arg}"; fi
      fi
    done
    ;;
  *)
    echo "unexpected systemctl verb: ${verb}" >&2
    exit 64
    ;;
esac
"#;
