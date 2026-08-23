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
import { createFizzyCards } from "./fizzy-cards.js";
import { createJjClient, diffFilePaths, isFullChangeId } from "./jj.js";
import { resolveBinary } from "./resolve-binary.js";
import { xdgDataDir } from "./xdg.js";

const REPO_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

// How many fetch→rebase→push cycles approve runs before conceding the race
// to whoever keeps landing first (DW-002 §6: "retry the loop a few times;
// if it still loses, make it a round").
const PUSH_ATTEMPTS = 3;

export function defaultChangeDir() {
  return path.join(xdgDataDir(), "skiff-bridge", "changes");
}

function escapeHtml(text) {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

// The comment approve leaves on the card: what landed, factually. Closing
// the card stays a human act in the web UI — landing code and declaring a
// feature done are different judgments (DW-002 §6).
function landedComment(change) {
  const rounds = change.rounds.length;
  const title = change.title ? `${escapeHtml(change.title)} — ` : "";
  return (
    `<p>Landed: ${title}${rounds} round${rounds === 1 ? "" : "s"} of ` +
    `${escapeHtml(change.repo)} change #${change.card}, tip ${escapeHtml(change.landed.tip.slice(0, 12))}.</p>`
  );
}

export function createChanges(config, { defaultCwd }) {
  const dir = config.dir ?? process.env.SKIFF_BRIDGE_CHANGE_DIR ?? defaultChangeDir();
  const reposDir = config.reposDir ?? process.env.SKIFF_BRIDGE_REPOS_DIR ?? defaultCwd;
  const remote = config.remote ?? "origin";
  // Resolved at boot like the harness binaries (see lib/resolve-binary.js):
  // the systemd unit's PATH has no ~/.cargo/bin, and a jj that only resolves
  // in interactive shells would fail on the first round instead of at start.
  const jj = createJjClient(resolveBinary("jj", config.binary ?? process.env.JJ_BINARY));
  const store = createChangeStore({ dir });
  const fizzy = createFizzyCards(config.fizzy ?? {});
  // One in-flight landing per change, awaitable — shutdown drains these so
  // a bridge restart never abandons a land mid-push.
  const landings = new Map();

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

  // The landing itself: fetch → rebase → conflict check → push, retried on
  // push races. Ends by recording the outcome in the store; only an
  // unclassified failure escapes (approve's catch records those).
  async function land(repoName, repoPath, card, change) {
    const first = change.rounds[0].changeId;
    const last = change.rounds.at(-1).changeId;
    let lastPushError = null;
    for (let attempt = 1; attempt <= PUSH_ATTEMPTS; attempt++) {
      await jj.fetch(repoPath, remote);
      await jj.rebaseOnto(repoPath, first, `main@${remote}`);
      const conflicts = await jj.conflictedIn(repoPath, first, last);
      if (conflicts.length > 0) {
        // Not an error state: the conflicted commits are what the agent
        // resolves, and "here is a reason to revise" is already the only
        // mechanism the system has (DW-002 §6).
        await store.failLanding(repoName, card, {
          reason: "the rebase onto main conflicts; resolve it as the next round",
          conflicts,
        });
        return;
      }
      await jj.setBookmark(repoPath, "main", last);
      try {
        await jj.push(repoPath, remote, "main");
      } catch (err) {
        lastPushError = err; // someone landed between our fetch and push — go around
        continue;
      }
      // From here the push has happened — the irreversible half is done, so
      // nothing below may turn the outcome into a failure. A tip lookup
      // that breaks degrades to an unresolved tip, not an unlanded change.
      let tip = "(unresolved)";
      try {
        const { commit } = await jj.show(repoPath, last);
        tip = commit?.commitId ?? tip;
      } catch (err) {
        console.error(`skiff-bridge: landed ${repoName}/${card} but could not resolve its tip:`, err.message);
      }
      const landed = await store.completeLanding(repoName, card, { tip });
      // Land first, then write to the card: a comment failure is recorded
      // on the change and never un-ships it.
      try {
        await fizzy.commentOnCard(card, landedComment(landed));
        await store.recordCardComment(repoName, card, { ok: true });
      } catch (err) {
        await store.recordCardComment(repoName, card, { ok: false, message: err.message });
      }
      return;
    }
    await store.failLanding(repoName, card, {
      reason: `push lost the race ${PUSH_ATTEMPTS} times: ${lastPushError?.message ?? "unknown"}`,
    });
  }

  return {
    async list() {
      return store.list();
    },

    async create(repoName, cardValue, { title = null, session = null } = {}) {
      const card = parseCard(cardValue);
      await resolveRepo(repoName);
      if (title !== null && (typeof title !== "string" || title.trim() === "")) {
        throw new HttpError(400, "title must be a non-empty string");
      }
      try {
        return await store.create(repoName, card, { title, session });
      } catch (err) {
        if (err.code === "EEXIST") throw new HttpError(409, err.message);
        throw err;
      }
    },

    async setSession(repoName, cardValue, session) {
      const card = parseCard(cardValue);
      await resolveRepo(repoName);
      if (typeof session !== "string" || session === "") {
        throw new HttpError(400, 'binding requires a non-empty "session"');
      }
      const change = await store.setSession(repoName, card, session);
      if (change === null) throw new HttpError(404, `no change ${repoName}/${card}`);
      return change;
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
    async addRound(repoName, cardValue, { author, changeId, note, gatesRan, worthKnowing }) {
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
      for (const [name, list] of [
        ["gatesRan", gatesRan],
        ["worthKnowing", worthKnowing],
      ]) {
        if (list !== undefined && (!Array.isArray(list) || list.some((s) => typeof s !== "string" || s.trim() === ""))) {
          throw new HttpError(400, `${name} must be an array of non-empty strings`);
        }
      }
      let round;
      try {
        round = await store.addRound(
          repoName,
          card,
          { author, changeId, note: note ?? null, gatesRan: gatesRan ?? [], worthKnowing: worthKnowing ?? [] },
          async (change) => {
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

    // Request changes (verb two of three, DW-002 §5): the note goes to the
    // change's bound agent session and the change returns to working; round
    // n+1 is the answer. `prompt` is the caller's delivery into the session
    // (server.js resolves the harness) and runs before the store records
    // anything: a note that never reached the agent must not reopen the
    // change. The reverse crash — prompted but not recorded — leaves the
    // change visibly in_review, and re-requesting is harmless.
    async requestChanges(repoName, cardValue, note, prompt) {
      const card = parseCard(cardValue);
      await resolveRepo(repoName);
      if (typeof note !== "string" || note.trim() === "") {
        throw new HttpError(400, 'request_changes requires a non-empty "note"');
      }
      const change = await store.get(repoName, card);
      if (change === null) throw new HttpError(404, `no change ${repoName}/${card}`);
      if (change.state !== "in_review") {
        throw new HttpError(409, `change ${repoName}/${card} is ${change.state}; only in_review changes take requests`);
      }
      if (change.session === null) {
        throw new HttpError(409, `change ${repoName}/${card} has no bound session to send the note to`);
      }
      await prompt(change.session);
      try {
        return await store.requestChanges(repoName, card, note);
      } catch (err) {
        if (err.code === "TRANSITION") throw new HttpError(409, err.message);
        throw err;
      }
    },

    // Approve (verb one, DW-002 §6): fetch origin/main, rebase the rounds
    // onto it, push — that is the entire mechanism, and it produces the
    // same artifact as merging a PR. Approve is a request, not an instant:
    // this transitions to `landing` and answers immediately; the land runs
    // async and ends in `shipped`, or back in `in_review` carrying the
    // reason (a conflict for the agent to resolve as the next round, or a
    // push race lost PUSH_ATTEMPTS times). The state machine makes approve
    // unavailable while already landing, and the Fizzy comment happens
    // strictly after the land — the land is the valuable, irreversible
    // half; the card annotation is recoverable metadata.
    async approve(repoName, cardValue) {
      const card = parseCard(cardValue);
      const repoPath = await resolveRepo(repoName);
      let change;
      try {
        change = await store.transition(repoName, card, "landing");
      } catch (err) {
        if (err.code === "TRANSITION") throw new HttpError(409, err.message);
        throw err;
      }
      if (change === null) throw new HttpError(404, `no change ${repoName}/${card}`);
      const key = `${repoName}/${card}`;
      const landing = land(repoName, repoPath, card, change)
        .catch(async (err) => {
          // A failure the loop did not classify (jj itself broke, the store
          // could not append). Recorded on the change if at all possible;
          // the console is the fallback, never the only record by intent.
          console.error(`skiff-bridge: landing ${key} failed:`, err);
          await store.failLanding(repoName, card, { reason: `landing failed: ${err.message}` }).catch(() => {});
        })
        .finally(() => landings.delete(key));
      landings.set(key, landing);
      return change;
    },

    // The in-flight landing for a change, if any — resolved otherwise.
    // close() drains these so a bridge shutdown never abandons a push.
    landSettled(repoName, cardValue) {
      return landings.get(`${repoName}/${cardValue}`) ?? Promise.resolve();
    },

    async shutdown() {
      await Promise.all(landings.values());
    },

    // The two lifecycle moves before approve: submit puts the change in
    // front of the human, reopen takes it back for another round.
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
  return {
    list: fail,
    create: fail,
    get: fail,
    diff: fail,
    addRound: fail,
    addAnnotation: fail,
    transition: fail,
    setSession: fail,
    requestChanges: fail,
    approve: fail,
    landSettled: () => Promise.resolve(),
    shutdown: async () => {},
  };
}
