// bridge/lib/file-tail.js
// Zero-dependency tail for a session file, used by the harness stream
// drivers to turn disk appends into events. pi and muse both write session
// files append-only (one JSONL entry per record) and pi rewrites on
// compaction, so the tail has exactly two jobs: emit complete new lines as
// they land, and detect a rewrite (file shrank or was replaced) so the
// consumer can reset to the whole file.
//
// Reads are deliberately synchronous: session files are small (a long
// session is still tens of KB), the watcher callback is a single macrotask
// so no two reads can overlap, and sync reads remove the promise-ordering
// races that an async re-entrant read would need guarding. The partial-line
// buffer is the standard JSONL discipline: a line without a trailing LF is
// kept until the rest arrives, so pi mid-write can never deliver a half
// entry.
//
// Delivery contract: `start()` performs the initial read — every complete
// line, flagged `initial: true` — and attaches the file watcher. When the
// file does not exist yet (the newborn-session window: pi buffers everything
// in memory until the first assistant message; muse creates the whole dated
// session directory only when the first run boots) it watches the deepest
// EXISTING ancestor directory and walks back down as path components appear,
// delivering the first read as `initial` once the file lands. Subsequent
// `change` events deliver only new lines (initial: false). A rewrite or
// truncation — compaction — delivers `onReset` followed by a fresh full read
// (initial: true), so the consumer rebuilds its state from byte zero and
// re-broadcasts a snapshot. `close()` stops watching; the consumer owns the
// callbacks' side effects.

import fs from "node:fs";
import path from "node:path";

const REWATCH_DELAY_MS = 100;

export function createFileTail(file, { onLines, onReset }) {
  let offset = 0;
  let partial = "";
  let fileWatcher = null;
  let dirWatcher = null;
  let closed = false;

  function statSize() {
    try {
      return fs.statSync(file).size;
    } catch {
      return null; // missing file: the dir watcher owns the wait
    }
  }

  // Read from the tail's current offset, deliver the complete new lines as
  // one batch, advance the offset. Returns true on success; on a transient
  // read error nothing is delivered and the offset is kept, so the next
  // watch event retries the same bytes. Shrink detection is the caller's
  // job (see readNewOrReset).
  function readNew(initial) {
    const size = statSize();
    if (size === null || size === offset) return true;
    let fd;
    try {
      fd = fs.openSync(file, "r");
      const buffer = Buffer.alloc(size - offset);
      fs.readSync(fd, buffer, 0, buffer.length, offset);
      const text = buffer.toString("utf8");
      const merged = partial + text;
      const lines = [];
      let start = 0;
      let nl;
      while ((nl = merged.indexOf("\n", start)) !== -1) {
        let line = merged.slice(start, nl);
        start = nl + 1;
        if (line.endsWith("\r")) line = line.slice(0, -1);
        lines.push(line);
      }
      partial = merged.slice(start);
      offset = size;
      if (lines.length > 0) onLines(lines, initial);
      return true;
    } catch {
      return false;
    } finally {
      if (fd !== undefined) fs.closeSync(fd);
    }
  }

  // The shared entry point for every wake-up (watch event or scan): detect a
  // truncation — the file shrank under our offset, which only compaction can
  // do — and reset; otherwise deliver whatever is new.
  function readNewOrReset() {
    const size = statSize();
    if (size === null) return; // vanished: the rename path owns the wait
    if (size < offset) {
      resetAndReRead();
      return;
    }
    readNew(false);
  }

  // The rewrite path: the file was truncated or replaced (compaction), so
  // every stored offset is meaningless. Reset, then re-read the whole file
  // as one initial delivery. A missing file hands the wait back to the dir
  // watcher; a failed read leaves the offset at zero so the next watch event
  // retries the full read.
  function resetAndReRead() {
    offset = 0;
    partial = "";
    onReset();
    if (statSize() === null) {
      attachDirWatcher();
      return;
    }
    readNew(true);
  }

  function attachFileWatcher() {
    if (closed) return;
    if (dirWatcher) {
      dirWatcher.close();
      dirWatcher = null;
    }
    fileWatcher = fs.watch(file, (eventType) => {
      if (closed) return;
      if (eventType === "rename") {
        // The path was replaced or removed. A present file is a rewrite
        // (compaction); a missing one starts the unborn wait.
        fileWatcher?.close();
        fileWatcher = null;
        if (statSize() === null) attachDirWatcher();
        else {
          resetAndReRead();
          attachFileWatcher();
        }
        return;
      }
      readNewOrReset();
    });
    fileWatcher.on("error", () => {
      // A watcher error (e.g. the inotify watch died) must not strand the
      // stream silently; re-watch once after a beat, else give up quietly.
      fileWatcher?.close();
      fileWatcher = null;
      if (!closed) setTimeout(attachFileWatcher, REWATCH_DELAY_MS).unref?.();
    });
  }

  // The unborn-session window: the file — possibly its whole directory chain
  // (muse creates sessions/YYYY/MM/DD/<uuid>/ only when the first run boots)
  // — does not exist yet. Watch the deepest EXISTING ancestor and re-evaluate
  // on every event in it: when a deeper component appears the watch moves
  // down, and when the file itself appears the initial read is delivered and
  // the file watcher takes over.
  function deepestExistingAncestor() {
    let dir = path.dirname(file);
    while (!fs.existsSync(dir)) {
      const parent = path.dirname(dir);
      if (parent === dir) break; // the filesystem root always exists
      dir = parent;
    }
    return dir;
  }

  function attachDirWatcher() {
    if (closed) return;
    const dir = deepestExistingAncestor();
    let watcher;
    try {
      watcher = fs.watch(dir, () => {
        if (closed) return;
        reevaluateUnborn(watcher, dir);
      });
    } catch {
      // The ancestor vanished between the existence check and the attach;
      // retry after a beat — its own parent will be found then.
      if (!closed) setTimeout(attachDirWatcher, REWATCH_DELAY_MS).unref?.();
      return;
    }
    dirWatcher = watcher;
    watcher.on("error", () => {
      watcher.close();
      if (dirWatcher === watcher) dirWatcher = null;
      if (!closed) setTimeout(attachDirWatcher, REWATCH_DELAY_MS).unref?.();
    });
    // The path may have appeared between the existence check and the attach;
    // re-evaluate immediately so the appearance is never missed.
    reevaluateUnborn(watcher, dir);
  }

  function reevaluateUnborn(watcher, watchedDir) {
    if (closed || dirWatcher !== watcher) return;
    if (statSize() !== null) {
      watcher.close();
      dirWatcher = null;
      readNewOrReset();
      // Attach the file watcher regardless of read outcome: a failed
      // initial read leaves the offset at zero and the next change event
      // retries the full read. The watch must not stay closed while the
      // file exists.
      attachFileWatcher();
      return;
    }
    if (deepestExistingAncestor() !== watchedDir) {
      // A deeper path component appeared; move the watch down to it.
      watcher.close();
      dirWatcher = null;
      attachDirWatcher();
    }
  }

  return {
    start() {
      if (statSize() === null) attachDirWatcher();
      else {
        readNew(true);
        attachFileWatcher();
      }
    },
    // Read whatever is new right now, synchronously. Used by the registry on
    // agent_end/agent_settled, where the file write and the agent event can
    // race the watcher: a forced scan closes the window deterministically.
    scan() {
      if (closed) return;
      readNewOrReset();
    },
    close() {
      closed = true;
      fileWatcher?.close();
      dirWatcher?.close();
      fileWatcher = null;
      dirWatcher = null;
    },
  };
}
