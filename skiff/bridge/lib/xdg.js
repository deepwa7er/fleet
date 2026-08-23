// bridge/lib/xdg.js
// XDG base-directory resolution shared by everything in the bridge that
// keeps or scans per-user data: $XDG_DATA_HOME when set and non-empty,
// falling back to ~/.local/share per the spec. Muse's session store and the
// change store both resolve through this so the phone and the CLI (and the
// tests) always agree on where data lives.

import path from "node:path";
import os from "node:os";

export function xdgDataDir() {
  const dataHome = process.env.XDG_DATA_HOME;
  return dataHome && dataHome.trim() !== "" ? dataHome : path.join(os.homedir(), ".local", "share");
}
