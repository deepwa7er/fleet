// bridge/lib/jj.js
// Read-only jj shell-outs for the change object (DW-002 §4): resolve a
// change id to its commit metadata, and render diffs in git format. Rounds
// are jj commits, and a jj change id is the stable handle that survives
// every amend and rebase — which is exactly why the store keeps change ids
// and asks this module for the volatile parts (commit id, description,
// parents) on demand.
//
// Every read runs with --ignore-working-copy so a bridge read never
// snapshots the working copy or takes the operation lock out from under a
// human (or an agent) mid-edit, and with --color never so output is data,
// not terminal art.
//
// The mutating verbs (fetch, rebase, bookmark set, push — approve's
// mechanics, DW-002 §6) deliberately do NOT pass --ignore-working-copy:
// they participate in jj's normal snapshot-and-checkout discipline, exactly
// as if a human had run them. Skipping the snapshot would leave a stale
// working copy behind whenever the rebase moves commits the checkout sits
// on, which surfaces to the human as a "stale working copy" error later.
// Every mutation lands in the operation log and is jj-undoable.

import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

// jj change ids are reversed hex: sixteen digits drawn from k–z. A full id
// is 32 characters, and the store only ever holds full ids — prefixes would
// make the parentage comparison in lib/changes.js ambiguous.
const CHANGE_ID = /^[k-z]{32}$/;

// One record per commit, fields joined by US (0x1f) — a control character
// that cannot appear in an email, a timestamp, or a first line of a
// description that jj itself accepted.
const SHOW_TEMPLATE =
  'change_id ++ "\\x1f" ++ commit_id ++ "\\x1f" ++ description.first_line() ++ "\\x1f" ++ author.email() ++ "\\x1f" ++ committer.timestamp().format("%Y-%m-%dT%H:%M:%S%z") ++ "\\x1f" ++ parents.map(|c| c.change_id()).join(",") ++ "\\n"';

export function isFullChangeId(value) {
  return typeof value === "string" && CHANGE_ID.test(value);
}

export function createJjClient(binaryPath) {
  async function run(repoPath, args, { snapshot = false } = {}) {
    const flags = snapshot ? [] : ["--ignore-working-copy"];
    return execFileAsync(binaryPath, [...flags, "--color", "never", ...args], {
      cwd: repoPath,
      maxBuffer: 64 * 1024 * 1024, // a cumulative diff of a large round set is bigger than Node's 1 MiB default
    });
  }

  // "Revision doesn't exist" is an answer (null), not a failure: an
  // abandoned round or a typo'd id both surface as an absent commit, and
  // the caller decides how loud to be about it.
  function isUnknownRevision(err) {
    return typeof err?.stderr === "string" && err.stderr.includes("doesn't exist");
  }

  return {
    // Resolve one change id to its commit metadata, or null when no visible
    // commit carries it. A divergent change id (two visible commits after a
    // concurrent amend) is reported as such rather than picking a winner —
    // the store must never silently bind a round to the wrong commit.
    async show(repoPath, changeId) {
      if (!isFullChangeId(changeId)) throw new Error(`not a full jj change id: ${changeId}`);
      let stdout;
      try {
        ({ stdout } = await run(repoPath, ["log", "--no-graph", "-r", changeId, "-T", SHOW_TEMPLATE]));
      } catch (err) {
        if (isUnknownRevision(err)) return { commit: null, divergent: false };
        throw new Error(`jj log failed in ${repoPath}: ${err.stderr?.trim() || err.message}`);
      }
      const records = stdout.split("\n").filter((line) => line !== "");
      if (records.length === 0) return { commit: null, divergent: false };
      if (records.length > 1) return { commit: null, divergent: true };
      const [id, commitId, description, authorEmail, timestamp, parents] = records[0].split("\x1f");
      return {
        commit: {
          changeId: id,
          commitId,
          description,
          authorEmail,
          timestamp,
          parents: parents === "" ? [] : parents.split(","),
        },
        divergent: false,
      };
    },

    // The diff introduced by one round: everything between the commit's
    // parents and the commit, in git format (what the review renders and
    // what annotation positions refer to).
    async diffForRound(repoPath, changeId) {
      if (!isFullChangeId(changeId)) throw new Error(`not a full jj change id: ${changeId}`);
      try {
        const { stdout } = await run(repoPath, ["diff", "-r", changeId, "--git"]);
        return stdout;
      } catch (err) {
        throw new Error(`jj diff failed in ${repoPath}: ${err.stderr?.trim() || err.message}`);
      }
    },

    // --- Mutating verbs: approve's mechanics (DW-002 §6) -------------------

    // Refresh the remote-tracking view. Push safety depends on this being
    // recent, but not on it being current — jj pushes with expected-old-
    // value semantics, so a stale view fails the push rather than
    // clobbering what someone else landed.
    async fetch(repoPath, remote) {
      try {
        await run(repoPath, ["git", "fetch", "--remote", remote], { snapshot: true });
      } catch (err) {
        throw new Error(`jj git fetch failed in ${repoPath}: ${err.stderr?.trim() || err.message}`);
      }
    },

    // Rebase the whole stack rooted at round 1 onto the fetched main. jj
    // records conflicts inside the rebased commits instead of stopping, so
    // this always completes — the caller inspects conflictedIn() next. -s
    // moves the root and every descendant, so a stray child the agent left
    // on the stack moves along with it instead of being orphaned.
    async rebaseOnto(repoPath, rootChangeId, destination) {
      if (!isFullChangeId(rootChangeId)) throw new Error(`not a full jj change id: ${rootChangeId}`);
      try {
        await run(repoPath, ["rebase", "-s", rootChangeId, "-d", destination], { snapshot: true });
      } catch (err) {
        throw new Error(`jj rebase failed in ${repoPath}: ${err.stderr?.trim() || err.message}`);
      }
    },

    // The change ids in first::last whose commits carry conflicts.
    async conflictedIn(repoPath, firstChangeId, lastChangeId) {
      if (!isFullChangeId(firstChangeId)) throw new Error(`not a full jj change id: ${firstChangeId}`);
      if (!isFullChangeId(lastChangeId)) throw new Error(`not a full jj change id: ${lastChangeId}`);
      try {
        const { stdout } = await run(repoPath, [
          "log",
          "--no-graph",
          "-r",
          `(${firstChangeId}::${lastChangeId}) & conflicts()`,
          "-T",
          'change_id ++ "\\n"',
        ]);
        return stdout.split("\n").filter((line) => line !== "");
      } catch (err) {
        throw new Error(`jj log failed in ${repoPath}: ${err.stderr?.trim() || err.message}`);
      }
    },

    // Point the local bookmark at the tip round. --allow-backwards because a
    // previous failed landing may have left it on a now-abandoned attempt;
    // the local bookmark is scaffolding, the push is what is race-checked.
    async setBookmark(repoPath, name, changeId) {
      if (!isFullChangeId(changeId)) throw new Error(`not a full jj change id: ${changeId}`);
      try {
        await run(repoPath, ["bookmark", "set", name, "-r", changeId, "--allow-backwards"], { snapshot: true });
      } catch (err) {
        throw new Error(`jj bookmark set failed in ${repoPath}: ${err.stderr?.trim() || err.message}`);
      }
    },

    // Push the bookmark. Throws on any failure — a concurrent land shows up
    // here as jj's stale-info rejection, and jj refuses to push conflicted
    // commits at all, which backstops the conflictedIn() check.
    async push(repoPath, remote, bookmark) {
      try {
        await run(repoPath, ["git", "push", "--remote", remote, "--bookmark", bookmark], { snapshot: true });
      } catch (err) {
        throw new Error(`jj git push failed in ${repoPath}: ${err.stderr?.trim() || err.message}`);
      }
    },

    // The cumulative diff: the feature as it now stands, from the base of
    // round 1 (its parent) to the tip round. Falls out for free because
    // rounds are additive (DW-002 §4).
    async diffCumulative(repoPath, firstChangeId, lastChangeId) {
      if (!isFullChangeId(firstChangeId)) throw new Error(`not a full jj change id: ${firstChangeId}`);
      if (!isFullChangeId(lastChangeId)) throw new Error(`not a full jj change id: ${lastChangeId}`);
      try {
        const { stdout } = await run(repoPath, [
          "diff",
          "--from",
          `${firstChangeId}-`,
          "--to",
          lastChangeId,
          "--git",
        ]);
        return stdout;
      } catch (err) {
        throw new Error(`jj diff failed in ${repoPath}: ${err.stderr?.trim() || err.message}`);
      }
    },
  };
}

// The files a git-format diff touches — both sides of every rename, so an
// annotation can anchor to the old path of a deletion as well as the new
// path of an addition. Used to reject annotations that point at files a
// round never changed.
export function diffFilePaths(diffText) {
  const paths = new Set();
  for (const line of diffText.split("\n")) {
    // `jj diff --git` headers are `diff --git a/<path> b/<path>`; paths with
    // spaces are not quoted by jj (it follows git's default), so split on
    // the ` b/` boundary rather than whitespace.
    if (!line.startsWith("diff --git a/")) continue;
    const rest = line.slice("diff --git a/".length);
    const boundary = rest.indexOf(" b/");
    if (boundary === -1) continue;
    paths.add(rest.slice(0, boundary));
    paths.add(rest.slice(boundary + " b/".length));
  }
  return paths;
}
