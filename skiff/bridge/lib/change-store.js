// bridge/lib/change-store.js
// Durable store for change objects (DW-002 §4): one card, one change, an
// ordered additive sequence of rounds, annotations positioned in a round's
// diff, and a small state machine. This is the first durable state the
// bridge has ever owned — the design gives annotations to the bridge
// because they are "the one thing with no other home", and the change
// record (state, round order, notes) lives with them so the repository
// keeps holding code and nothing else.
//
// Storage is one append-only JSONL event log per change:
//
//   <dir>/<repo>/<card>.jsonl
//   {"event":"created","repo":"fleet","card":81,"at":…}
//   {"event":"round","n":1,"author":"agent","changeId":…,"note":…,"at":…}
//   {"event":"annotation","id":…,"round":1,"path":…,"line":…,"side":…,"text":…,"at":…}
//   {"event":"state","state":"in_review","at":…}
//
// Append-only mirrors the model itself (rounds are additive, never
// amended), gives history for free, and reuses the discipline the bridge
// already applies to harness JSONL: every line parses independently, and an
// unparseable last line — a crash mid-append — is skipped, because a write
// that was never acknowledged never happened. Appends are fsync'd before
// they are acknowledged; annotations are authored output, not cache.
//
// This module knows nothing about jj or HTTP. Validation that needs the
// repository (does the change id exist, is round n+1 a child of round n)
// lives in lib/changes.js; this file owns durability, replay, and the
// transitions that are legal at all.

import { promises as fs } from "node:fs";
import path from "node:path";

// working → in_review → landing → shipped, with the two returns the design
// names: request-changes (in_review → working) and a landing that fails
// back into review carrying a conflict round (landing → in_review). The
// landing and shipped states exist now so step 03's approve only adds
// mechanics, not model — but nothing in step 02 enters them.
const TRANSITIONS = {
  working: ["in_review"],
  in_review: ["working", "landing"],
  landing: ["shipped", "in_review"],
  shipped: [],
};

export const STATES = Object.keys(TRANSITIONS);

// Rounds and annotations are authored while the change is alive; once it is
// landing (the diff under approval must not mutate) or shipped, the record
// is frozen.
const APPENDABLE_STATES = ["working", "in_review"];

const AUTHORS = ["agent", "you"];

// Repo and card become path segments, so they are validated even though
// lib/changes.js validates them first — the store must be safe on its own.
const REPO_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

function assertRepo(repo) {
  if (typeof repo !== "string" || !REPO_NAME.test(repo)) {
    throw new Error(`invalid repo name: ${JSON.stringify(repo)}`);
  }
}

function assertCard(card) {
  if (!Number.isInteger(card) || card < 1) {
    throw new Error(`invalid card number: ${JSON.stringify(card)}`);
  }
}

function parseLine(line) {
  try {
    return JSON.parse(line.endsWith("\r") ? line.slice(0, -1) : line);
  } catch {
    return null; // a crash mid-append; the write was never acknowledged
  }
}

// Fold the event log into the current change object. Unknown event types
// are skipped rather than fatal: an older bridge reading a log a newer one
// wrote should degrade to the fields it knows, not refuse the change.
function replay(events) {
  let change = null;
  for (const event of events) {
    if (event?.event === "created") {
      change = {
        repo: event.repo,
        card: event.card,
        title: event.title ?? null,
        session: event.session ?? null,
        state: "working",
        createdAt: event.at,
        updatedAt: event.at,
        rounds: [],
        lastRequest: null,
        landed: null,
        lastLanding: null,
        cardComment: null,
      };
      continue;
    }
    if (change === null) continue; // garbage before "created" — not a change log
    if (event.event === "round") {
      change.rounds.push({
        n: event.n,
        author: event.author,
        changeId: event.changeId,
        note: event.note ?? null,
        gatesRan: event.gatesRan ?? [],
        worthKnowing: event.worthKnowing ?? [],
        createdAt: event.at,
        annotations: [],
      });
    } else if (event.event === "session") {
      change.session = event.session;
    } else if (event.event === "requested") {
      change.lastRequest = { note: event.note, at: event.at };
    } else if (event.event === "landed") {
      change.landed = { tip: event.tip, at: event.at };
      change.lastLanding = { ok: true, at: event.at };
    } else if (event.event === "landing_failed") {
      change.lastLanding = { ok: false, reason: event.reason, conflicts: event.conflicts ?? [], at: event.at };
    } else if (event.event === "card_comment") {
      change.cardComment = { ok: event.ok, ...(event.message ? { message: event.message } : {}), at: event.at };
    } else if (event.event === "annotation") {
      const round = change.rounds.find((r) => r.n === event.round);
      if (round) {
        round.annotations.push({
          id: event.id,
          path: event.path,
          line: event.line,
          side: event.side,
          text: event.text,
          createdAt: event.at,
        });
      }
    } else if (event.event === "state") {
      change.state = event.state;
    } else {
      continue; // unknown event type — skip, do not bump updatedAt
    }
    change.updatedAt = event.at;
  }
  return change;
}

export function createChangeStore({ dir }) {
  if (typeof dir !== "string" || dir === "") {
    throw new Error("change store requires a directory");
  }

  // One in-process queue per change file: the bridge is a single process
  // (systemd unit), so serializing read-modify-append per change is enough
  // to keep "load, validate against current state, append" atomic.
  const queues = new Map();
  function serialize(key, task) {
    const tail = queues.get(key) ?? Promise.resolve();
    const next = tail.then(task, task);
    // Keep the chain from growing without bound once it settles.
    queues.set(
      key,
      next.then(
        () => {},
        () => {}
      )
    );
    return next;
  }

  function filePath(repo, card) {
    assertRepo(repo);
    assertCard(card);
    return path.join(dir, repo, `${card}.jsonl`);
  }

  async function readEvents(repo, card) {
    let raw;
    try {
      raw = await fs.readFile(filePath(repo, card), "utf8");
    } catch (err) {
      if (err.code === "ENOENT") return null;
      throw err;
    }
    return raw
      .split("\n")
      .filter((line) => line !== "")
      .map(parseLine)
      .filter((event) => event !== null);
  }

  async function load(repo, card) {
    const events = await readEvents(repo, card);
    if (events === null) return null;
    return replay(events);
  }

  // Append one event and fsync before acknowledging. "exclusive" makes the
  // open fail if the file exists — that is what turns two concurrent
  // creates into one 'already exists' instead of a clobber.
  async function append(repo, card, event, { exclusive = false } = {}) {
    const target = filePath(repo, card);
    await fs.mkdir(path.dirname(target), { recursive: true });
    const handle = await fs.open(target, exclusive ? "wx" : "a");
    try {
      await handle.write(JSON.stringify(event) + "\n");
      await handle.sync();
    } finally {
      await handle.close();
    }
  }

  return {
    // Create the change record for a card. Exactly one change per card per
    // repo — the card number is the only identifier the user sees, so a
    // second change under the same number would be two things wearing one
    // name.
    async create(repo, card, { title = null, session = null } = {}) {
      if (title !== null && (typeof title !== "string" || title.trim() === "")) {
        throw new Error("title must be a non-empty string");
      }
      if (session !== null && (typeof session !== "string" || session === "")) {
        throw new Error("session must be a non-empty string");
      }
      return serialize(filePath(repo, card), async () => {
        const event = { event: "created", repo, card, title, session, at: new Date().toISOString() };
        try {
          await append(repo, card, event, { exclusive: true });
        } catch (err) {
          if (err.code === "EEXIST") {
            const conflict = new Error(`change ${repo}/${card} already exists`);
            conflict.code = "EEXIST";
            throw conflict;
          }
          throw err;
        }
        return replay([event]);
      });
    },

    // The current change object, or null.
    async get(repo, card) {
      return serialize(filePath(repo, card), () => load(repo, card));
    },

    // Every change in the store, newest activity first. The store holds
    // active work, not history — DW-002 §11 warns against letting it drift
    // into a permanent cross-system record — so a full scan is the honest
    // cost model and stays cheap.
    async list() {
      let repos;
      try {
        repos = await fs.readdir(dir, { withFileTypes: true });
      } catch (err) {
        if (err.code === "ENOENT") return [];
        throw err;
      }
      const changes = [];
      for (const entry of repos) {
        if (!entry.isDirectory()) continue;
        for (const file of await fs.readdir(path.join(dir, entry.name))) {
          if (!file.endsWith(".jsonl")) continue;
          const card = Number(file.slice(0, -".jsonl".length));
          if (!Number.isInteger(card)) continue;
          const change = await serialize(filePath(entry.name, card), () => load(entry.name, card));
          if (change) changes.push(change);
        }
      }
      changes.sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : -1));
      return changes;
    },

    // Append round n+1. `validate` runs inside the queue with the current
    // change, after the state check — lib/changes.js uses it for the jj
    // checks (the id exists, the round is a child of its predecessor) so
    // validation and append cannot interleave with another writer.
    async addRound(
      repo,
      card,
      { author, changeId, note = null, gatesRan = [], worthKnowing = [] },
      validate = async () => {}
    ) {
      if (!AUTHORS.includes(author)) throw new Error(`author must be one of: ${AUTHORS.join(", ")}`);
      if (typeof changeId !== "string" || changeId === "") throw new Error("round requires a changeId");
      if (note !== null && typeof note !== "string") throw new Error("note must be a string");
      for (const [name, list] of [
        ["gatesRan", gatesRan],
        ["worthKnowing", worthKnowing],
      ]) {
        if (!Array.isArray(list) || list.some((item) => typeof item !== "string" || item.trim() === "")) {
          throw new Error(`${name} must be an array of non-empty strings`);
        }
      }
      return serialize(filePath(repo, card), async () => {
        const change = await load(repo, card);
        if (change === null) return null;
        if (!APPENDABLE_STATES.includes(change.state)) {
          const err = new Error(`change ${repo}/${card} is ${change.state}; rounds are frozen`);
          err.code = "FROZEN";
          throw err;
        }
        if (change.rounds.some((r) => r.changeId === changeId)) {
          const err = new Error(`change id ${changeId} is already round ${change.rounds.find((r) => r.changeId === changeId).n}`);
          err.code = "DUPLICATE";
          throw err;
        }
        await validate(change);
        const event = {
          event: "round",
          n: change.rounds.length + 1,
          author,
          changeId,
          note,
          gatesRan,
          worthKnowing,
          at: new Date().toISOString(),
        };
        await append(repo, card, event);
        return { n: event.n, author, changeId, note, gatesRan, worthKnowing, createdAt: event.at, annotations: [] };
      });
    },

    // Append an annotation to an existing round. Position validation that
    // needs the diff happens in `validate`, inside the queue.
    async addAnnotation(repo, card, { id, round, path: file, line, side, text }, validate = async () => {}) {
      if (typeof id !== "string" || id === "") throw new Error("annotation requires an id");
      if (!Number.isInteger(round) || round < 1) throw new Error("annotation requires a round number");
      if (typeof file !== "string" || file === "") throw new Error("annotation requires a path");
      if (!Number.isInteger(line) || line < 1) throw new Error("annotation line must be a positive integer");
      if (side !== "old" && side !== "new") throw new Error('annotation side must be "old" or "new"');
      if (typeof text !== "string" || text.trim() === "") throw new Error("annotation requires text");
      return serialize(filePath(repo, card), async () => {
        const change = await load(repo, card);
        if (change === null) return null;
        if (!APPENDABLE_STATES.includes(change.state)) {
          const err = new Error(`change ${repo}/${card} is ${change.state}; annotations are frozen`);
          err.code = "FROZEN";
          throw err;
        }
        const target = change.rounds.find((r) => r.n === round);
        if (!target) {
          const err = new Error(`change ${repo}/${card} has no round ${round}`);
          err.code = "NO_ROUND";
          throw err;
        }
        await validate(change, target);
        const event = { event: "annotation", id, round, path: file, line, side, text, at: new Date().toISOString() };
        await append(repo, card, event);
        return { id, round, path: file, line, side, text, createdAt: event.at };
      });
    },

    // Bind (or rebind) the agent session the review's request-changes notes
    // go to. Rebinding is deliberate: a card can outlive the session that
    // started it.
    async setSession(repo, card, session) {
      if (typeof session !== "string" || session === "") throw new Error("session must be a non-empty string");
      return serialize(filePath(repo, card), async () => {
        const change = await load(repo, card);
        if (change === null) return null;
        const event = { event: "session", session, at: new Date().toISOString() };
        await append(repo, card, event);
        return { ...change, session, updatedAt: event.at };
      });
    },

    // Request changes: record the note and hand the change back to the
    // agent in one append sequence — the note and the reopen must not be
    // separable, or a crash between them leaves a reopened change with no
    // record of why.
    async requestChanges(repo, card, note) {
      if (typeof note !== "string" || note.trim() === "") throw new Error("request requires a note");
      return serialize(filePath(repo, card), async () => {
        const change = await load(repo, card);
        if (change === null) return null;
        if (change.state !== "in_review") {
          const err = new Error(`change ${repo}/${card} is ${change.state}; only in_review changes take requests`);
          err.code = "TRANSITION";
          throw err;
        }
        const at = new Date().toISOString();
        await append(repo, card, { event: "requested", note, at });
        await append(repo, card, { event: "state", state: "working", at });
        return { ...change, state: "working", lastRequest: { note, at }, updatedAt: at };
      });
    },

    // The two ends of a landing (the async half of approve). Each records
    // the outcome and the resulting state in one queue task; the store does
    // not know how the landing was attempted, only how it ended.
    async completeLanding(repo, card, { tip }) {
      if (typeof tip !== "string" || tip === "") throw new Error("completeLanding requires the tip commit");
      return serialize(filePath(repo, card), async () => {
        const change = await load(repo, card);
        if (change === null) return null;
        if (change.state !== "landing") {
          const err = new Error(`change ${repo}/${card} is ${change.state}, not landing`);
          err.code = "TRANSITION";
          throw err;
        }
        const at = new Date().toISOString();
        await append(repo, card, { event: "landed", tip, at });
        await append(repo, card, { event: "state", state: "shipped", at });
        return { ...change, state: "shipped", landed: { tip, at }, updatedAt: at };
      });
    },

    async failLanding(repo, card, { reason, conflicts = [] }) {
      if (typeof reason !== "string" || reason === "") throw new Error("failLanding requires a reason");
      return serialize(filePath(repo, card), async () => {
        const change = await load(repo, card);
        if (change === null) return null;
        if (change.state !== "landing") {
          const err = new Error(`change ${repo}/${card} is ${change.state}, not landing`);
          err.code = "TRANSITION";
          throw err;
        }
        const at = new Date().toISOString();
        await append(repo, card, { event: "landing_failed", reason, conflicts, at });
        await append(repo, card, { event: "state", state: "in_review", at });
        return { ...change, state: "in_review", updatedAt: at };
      });
    },

    // The Fizzy comment is the recoverable half of approve (land first,
    // card second — DW-002 §6); its outcome is recorded either way so a
    // failed comment is visible on the change instead of lost in a log.
    async recordCardComment(repo, card, { ok, message = null }) {
      if (typeof ok !== "boolean") throw new Error("recordCardComment requires ok: boolean");
      return serialize(filePath(repo, card), async () => {
        const change = await load(repo, card);
        if (change === null) return null;
        const event = { event: "card_comment", ok, ...(message ? { message } : {}), at: new Date().toISOString() };
        await append(repo, card, event);
        return { ...change, updatedAt: event.at };
      });
    },

    // Move the change to a new state, validating against the transition
    // table. Returns the updated change, null for an unknown change; an
    // illegal transition throws with the states named — a client driving
    // the lifecycle wrong should hear exactly what it did.
    async transition(repo, card, state) {
      if (!STATES.includes(state)) throw new Error(`unknown state: ${JSON.stringify(state)}`);
      return serialize(filePath(repo, card), async () => {
        const change = await load(repo, card);
        if (change === null) return null;
        if (!TRANSITIONS[change.state].includes(state)) {
          const err = new Error(`change ${repo}/${card} is ${change.state}; cannot move to ${state}`);
          err.code = "TRANSITION";
          throw err;
        }
        if (state === "in_review" && change.rounds.length === 0) {
          const err = new Error(`change ${repo}/${card} has no rounds; nothing to review`);
          err.code = "TRANSITION";
          throw err;
        }
        const event = { event: "state", state, at: new Date().toISOString() };
        await append(repo, card, event);
        return { ...change, state, updatedAt: event.at };
      });
    },
  };
}
