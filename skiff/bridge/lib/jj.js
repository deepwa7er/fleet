// bridge/lib/jj.js
// Read-only jj shell-outs for the change object (DW-002 §4): resolve a
// change id to its commit metadata, and render diffs in git format. Rounds
// are jj commits, and a jj change id is the stable handle that survives
// every amend and rebase — which is exactly why the store keeps change ids
// and asks this module for the volatile parts (commit id, description,
// parents) on demand.
//
// Every command runs with --ignore-working-copy so a bridge read never
// snapshots the working copy or takes the operation lock out from under a
// human (or an agent) mid-edit, and with --color never so output is data,
// not terminal art. This module never mutates a repository: approve's
// rebase-and-push is step 03 and does not live here.

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
  async function run(repoPath, args) {
    return execFileAsync(binaryPath, ["--ignore-working-copy", "--color", "never", ...args], {
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
