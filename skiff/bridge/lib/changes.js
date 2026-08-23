// bridge/lib/changes.js
// The change subsystem (DW-002 §4): composes the durable store
// (lib/change-store.js) with the jj repository (lib/jj.js) and maps every
// failure to the HTTP surface. server.js routes call this and nothing else.
//
// Division of labour, matching the design's storage split:
//   - rounds are jj commits — the repository already versions them, so this
//     module stores only the change id and asks jj for the rest on demand;
//   - the card binding is the card number on the change record — Fizzy
//     learns what landed via a comment at approve time, which is step 03;
//   - annotations and the change lifecycle live in the store, the one piece
//     with no other home.
//
// Repositories are addressed by name, resolved under one root (the same
// ~/code the bridge already uses as its default cwd), and must be jj
// repositories — the redesign runs on colocated jj, and a name that
// resolves outside the root or to a plain git checkout is a client error
// worth rejecting loudly.

import { promises as fs } from "node:fs";
import path from "node:path";
import { randomUUID } from "node:crypto";
import { HttpError } from "./errors.js";
import { createChangeStore } from "./change-store.js";
import { createJjClient, diffFilePaths, isFullChangeId } from "./jj.js";
import { resolveBinary } from "./resolve-binary.js";
import { xdgDataDir } from "./xdg.js";

const REPO_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

export function defaultChangeDir() {
  return path.join(xdgDataDir(), "skiff-bridge", "changes");
}

export function createChanges(config, { defaultCwd }) {
  const dir = config.dir ?? process.env.SKIFF_BRIDGE_CHANGE_DIR ?? defaultChangeDir();
  const reposDir = config.reposDir ?? process.env.SKIFF_BRIDGE_REPOS_DIR ?? defaultCwd;
  // Resolved at boot like the harness binaries (see lib/resolve-binary.js):
  // the systemd unit's PATH has no ~/.cargo/bin, and a jj that only resolves
  // in interactive shells would fail on the first round instead of at start.
  const jj = createJjClient(resolveBinary("jj", config.binary ?? process.env.JJ_BINARY));
  const store = createChangeStore({ dir });

  async function resolveRepo(name) {
    if (typeof name !== "string" || !REPO_NAME.test(name)) {
      throw new HttpError(400, `invalid repo name: ${JSON.stringify(name)}`);
    }
    const repoPath = path.join(reposDir, name);
    try {
      await fs.access(path.join(repoPath, ".jj"));
    } catch {
      throw new HttpError(404, `no jj repository named ${name} under ${reposDir}`);
    }
    return repoPath;
  }

  function parseCard(value) {
    const card = Number(value);
    if (!Number.isInteger(card) || card < 1) {
      throw new HttpError(400, `invalid card number: ${JSON.stringify(value)}`);
    }
    return card;
  }

  // The wire shape: the stored change with each round enriched by the
  // repository — commit id, description, author, timestamp. A round whose
  // change id no longer resolves (abandoned, or divergent after a
  // concurrent amend) carries commit: null plus the reason, never a guess.
  async function enrich(repoPath, change) {
    const rounds = await Promise.all(
      change.rounds.map(async (round) => {
        const { commit, divergent } = await jj.show(repoPath, round.changeId);
        return { ...round, commit, ...(divergent ? { divergent: true } : {}) };
      })
    );
    return { ...change, rounds };
  }

  return {
    async list() {
      return store.list();
    },

    async create(repoName, cardValue) {
      const card = parseCard(cardValue);
      await resolveRepo(repoName);
      try {
        return await store.create(repoName, card);
      } catch (err) {
        if (err.code === "EEXIST") throw new HttpError(409, err.message);
        throw err;
      }
    },

    async get(repoName, cardValue) {
      const card = parseCard(cardValue);
      const repoPath = await resolveRepo(repoName);
      const change = await store.get(repoName, card);
      if (change === null) throw new HttpError(404, `no change ${repoName}/${card}`);
      return enrich(repoPath, change);
    },

    // Round n's diff (what changed since you last looked), or the
    // cumulative diff (the feature as it now stands) when no round is
    // named. Git format either way — annotation positions refer to it.
    async diff(repoName, cardValue, roundValue = null) {
      const card = parseCard(cardValue);
      const repoPath = await resolveRepo(repoName);
      const change = await store.get(repoName, card);
      if (change === null) throw new HttpError(404, `no change ${repoName}/${card}`);
      if (change.rounds.length === 0) throw new HttpError(404, `change ${repoName}/${card} has no rounds`);
      if (roundValue === null) {
        return jj.diffCumulative(repoPath, change.rounds[0].changeId, change.rounds.at(-1).changeId);
      }
      const n = Number(roundValue);
      const round = change.rounds.find((r) => r.n === n);
      if (!round) throw new HttpError(404, `change ${repoName}/${card} has no round ${roundValue}`);
      return jj.diffForRound(repoPath, round.changeId);
    },

    // Append round n+1. The jj checks run inside the store's write queue:
    // the change id must resolve to exactly one visible commit, and every
    // round after the first must be a child of its predecessor — rounds are
    // an ordered, additive stack, and a round that does not build on the
    // last one would silently break both diff views.
    async addRound(repoName, cardValue, { author, changeId, note }) {
      const card = parseCard(cardValue);
      const repoPath = await resolveRepo(repoName);
      if (!isFullChangeId(changeId)) {
        throw new HttpError(400, 'round requires "changeId": a full 32-character jj change id');
      }
      if (author !== "agent" && author !== "you") {
        throw new HttpError(400, 'round requires "author": "agent" or "you"');
      }
      if (note !== undefined && note !== null && typeof note !== "string") {
        throw new HttpError(400, "round note must be a string");
      }
      let round;
      try {
        round = await store.addRound(repoName, card, { author, changeId, note: note ?? null }, async (change) => {
          const { commit, divergent } = await jj.show(repoPath, changeId);
          if (divergent) throw new HttpError(409, `change id ${changeId} is divergent in ${repoName}`);
          if (commit === null) throw new HttpError(400, `change id ${changeId} does not exist in ${repoName}`);
          const previous = change.rounds.at(-1);
          if (previous && !commit.parents.includes(previous.changeId)) {
            throw new HttpError(
              400,
              `round ${change.rounds.length + 1} must be a child of round ${previous.n} (${previous.changeId}); ${changeId} is not`
            );
          }
        });
      } catch (err) {
        if (err.code === "FROZEN") throw new HttpError(409, err.message);
        if (err.code === "DUPLICATE") throw new HttpError(409, err.message);
        throw err;
      }
      if (round === null) throw new HttpError(404, `no change ${repoName}/${card}`);
      return round;
    },

    // Attach an annotation to a position in a round's diff. The file must
    // be one the round actually touched — an annotation pointing at a file
    // the diff never mentions is a client bug, and the review would render
    // it nowhere. (These are review-layer objects; they must never become
    // code comments — DW-002 §5.)
    async addAnnotation(repoName, cardValue, { round, path: file, line, side, text }) {
      const card = parseCard(cardValue);
      const repoPath = await resolveRepo(repoName);
      const n = Number(round);
      if (!Number.isInteger(n) || n < 1) throw new HttpError(400, 'annotation requires a "round" number');
      if (typeof file !== "string" || file === "") throw new HttpError(400, 'annotation requires a "path"');
      if (!Number.isInteger(line) || line < 1) throw new HttpError(400, 'annotation requires a positive "line"');
      const resolvedSide = side ?? "new";
      if (resolvedSide !== "old" && resolvedSide !== "new") {
        throw new HttpError(400, 'annotation "side" must be "old" or "new"');
      }
      if (typeof text !== "string" || text.trim() === "") throw new HttpError(400, 'annotation requires "text"');
      let annotation;
      try {
        annotation = await store.addAnnotation(
          repoName,
          card,
          { id: randomUUID(), round: n, path: file, line, side: resolvedSide, text },
          async (change, targetRound) => {
            const diff = await jj.diffForRound(repoPath, targetRound.changeId);
            if (!diffFilePaths(diff).has(file)) {
              throw new HttpError(400, `round ${n} of ${repoName}/${card} does not touch ${file}`);
            }
          }
        );
      } catch (err) {
        if (err.code === "FROZEN") throw new HttpError(409, err.message);
        if (err.code === "NO_ROUND") throw new HttpError(404, err.message);
        throw err;
      }
      if (annotation === null) throw new HttpError(404, `no change ${repoName}/${card}`);
      return annotation;
    },

    // The two lifecycle moves that exist before approve: submit puts the
    // change in front of the human, reopen takes it back for another round.
    // The landing → shipped path is step 03's approve and is deliberately
    // not reachable over HTTP yet — the store validates those transitions,
    // but nothing drives them.
    async transition(repoName, cardValue, state) {
      const card = parseCard(cardValue);
      await resolveRepo(repoName);
      let change;
      try {
        change = await store.transition(repoName, card, state);
      } catch (err) {
        if (err.code === "TRANSITION") throw new HttpError(409, err.message);
        throw err;
      }
      if (change === null) throw new HttpError(404, `no change ${repoName}/${card}`);
      return change;
    },
  };
}

// The degraded stand-in when the subsystem cannot construct (no jj binary
// on this host): every operation answers with the boot failure — visible,
// never silent — while the harness routes stay untouched. Mirrors
// createUnavailableHarness in server.js.
export function createUnavailableChanges(message) {
  const fail = () => {
    throw new HttpError(502, `change subsystem unavailable: ${message}`);
  };
  return { list: fail, create: fail, get: fail, diff: fail, addRound: fail, addAnnotation: fail, transition: fail };
}
