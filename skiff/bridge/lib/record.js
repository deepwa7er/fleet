// bridge/lib/record.js
// The ship-time export (DW-003 §2–3): when a landing completes, one
// self-contained JSON entry — the public subset of the change — is written
// into the record repository, committed, and pushed. Pushing is publishing.
//
// The privacy boundary lives HERE, in buildEntry, field by field, and
// exclusion is the default: a field the change object grows later does not
// reach the record until someone adds it to this function deliberately.
// Public: card, title, timestamps, commit/change ids, the author *kind*,
// the claims, the frozen diffs, the annotations. Private, never exported:
// round notes ("what prompted it"), request-changes notes, session ids,
// filesystem paths — and incidental internals like annotation ids.
//
// Failure discipline matches the Fizzy card comment: the land is the
// valuable, irreversible half, so an export that cannot write or push is
// recorded on the change (visible), never allowed to block or un-ship it.
// Exports are serialized in-process — two landings finishing together must
// not race a single git index.

import { promises as fs } from "node:fs";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import path from "node:path";
import os from "node:os";
import { resolveBinary } from "./resolve-binary.js";

const execFileAsync = promisify(execFile);

// Entries are committed by the bridge as itself — the record's authorship
// is honest ("written by the skiff bridge at ship time"), and the export
// must not depend on whatever user.email the checkout's owner configured.
const IDENTITY = ["-c", "user.name=skiff bridge", "-c", "user.email=skiff-bridge@deepwa7er.net"];

export function defaultRecordDir() {
  return path.join(os.homedir(), "code", "record");
}

// The public subset of a shipped change, per DW-003 §3. `diffsByRound`
// maps round n → git-format diff text, resolved by the caller at export
// time so the entry renders forever without the jj repository at hand.
export function buildEntry(change, diffsByRound) {
  return {
    repo: change.repo,
    card: change.card,
    title: change.title ?? null,
    landedAt: change.landed.at,
    tip: change.landed.tip,
    rounds: change.rounds.map((round) => ({
      n: round.n,
      author: round.author,
      changeId: round.changeId,
      commit: round.commit?.commitId ?? null,
      gatesRan: round.gatesRan ?? [],
      worthKnowing: round.worthKnowing ?? [],
      diff: diffsByRound.get(round.n) ?? null,
      annotations: (round.annotations ?? []).map((annotation) => ({
        path: annotation.path,
        line: annotation.line,
        side: annotation.side,
        text: annotation.text,
      })),
    })),
    afterward: [],
  };
}

export function createRecord(config = {}) {
  const dir = config.dir ?? process.env.SKIFF_BRIDGE_RECORD_DIR ?? defaultRecordDir();
  const remote = config.remote ?? "origin";
  const git = resolveBinary("git", config.binary ?? process.env.GIT_BINARY);

  async function run(args) {
    return execFileAsync(git, [...IDENTITY, ...args], { cwd: dir });
  }

  // One export at a time: a single promise chain, the same discipline the
  // change store applies per file — here per repository, because git's
  // index is one.
  let queue = Promise.resolve();
  function serialize(task) {
    const next = queue.then(task, task);
    queue = next.then(
      () => {},
      () => {}
    );
    return next;
  }

  return {
    // Write, commit, push one entry. Throws on any failure — the caller
    // records the outcome on the change either way.
    async export(change, diffsByRound) {
      return serialize(async () => {
        const entry = buildEntry(change, diffsByRound);
        const relative = path.join(entry.repo, `${entry.card}.json`);
        const target = path.join(dir, relative);
        await fs.mkdir(path.dirname(target), { recursive: true });
        await fs.writeFile(target, JSON.stringify(entry, null, 2) + "\n");
        try {
          await run(["add", relative]);
          await run(["commit", "-m", `record: ${entry.repo} #${entry.card}${entry.title ? ` — ${entry.title}` : ""}`]);
          await run(["push", remote, "HEAD"]);
        } catch (err) {
          throw new Error(`record export failed in ${dir}: ${err.stderr?.trim() || err.message}`);
        }
        return relative;
      });
    },
  };
}
