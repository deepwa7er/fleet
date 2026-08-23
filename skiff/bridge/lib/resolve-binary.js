// bridge/lib/resolve-binary.js
// A harness executable, resolved to an absolute path at boot.
//
// Why not just the bare name? The bridge runs under systemd user units (and
// launchd on the Mac), whose PATHs do not include ~/.local/bin — where pi and
// muse are actually installed on the desktop. A command name that only
// resolves for interactive shells would fail intermittently on the first
// prompt, so resolution happens once at boot: PATH first, then the
// home-relative locations that match where the CLIs install themselves. An
// explicit override is honored as-is when it carries a path; a bare name
// goes through the same search. Failing to find the executable is a boot
// error, like a missing password — a bridge that cannot prompt is not a
// bridge.

import path from "node:path";
import os from "node:os";
import fs from "node:fs";

export function resolveBinary(name, explicit) {
  const candidates =
    explicit && explicit.trim() !== ""
      ? [explicit]
      : [
          name,
          path.join(os.homedir(), ".local", "bin", name),
          path.join(os.homedir(), "bin", name),
          // Where `cargo install` puts binaries (jj on the desktop) — like
          // ~/.local/bin, present in interactive PATHs but not systemd's.
          path.join(os.homedir(), ".cargo", "bin", name),
        ];
  for (const candidate of candidates) {
    if (candidate.includes("/")) {
      if (fs.existsSync(candidate)) return path.resolve(candidate);
      continue;
    }
    for (const dir of (process.env.PATH ?? "").split(path.delimiter)) {
      if (dir === "") continue;
      const resolved = path.join(dir, candidate);
      if (fs.existsSync(resolved)) return resolved;
    }
  }
  throw new Error(
    `skiff-bridge: ${name} executable not found (tried: ${candidates.join(", ")}); set the ${name.toUpperCase()}_BINARY env to its absolute path`
  );
}
