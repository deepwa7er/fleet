// bridge/lib/fizzy-cards.js
// The one Fizzy operation approve needs: posting a comment to a card
// (DW-002 §6 — approve comments, it does not close; Fizzy's API cannot
// close a card, deliberately). Zero-dependency: Node's fetch against the
// contract documented in .agents/skills/fizzy/SKILL.md, the same one
// crates/fizzy speaks from Rust.
//
// Auth follows the fizzy CLI: a bearer token read from a file — deliberately
// not an env var — defaulting to ~/.config/fizzy/write-token, overridable by
// FIZZY_TOKEN_FILE. The token is read per call, not cached: the file can be
// provisioned or rotated while the bridge runs, and a comment happens at
// most once per landing.
//
// Fizzy 302s unauthenticated requests to its sign-in menu, so a redirect is
// a hard error naming the likely cause, never followed.

import { promises as fs } from "node:fs";
import path from "node:path";
import os from "node:os";

const TIMEOUT_MS = 15_000;

export function defaultTokenFile() {
  return process.env.FIZZY_TOKEN_FILE ?? path.join(os.homedir(), ".config", "fizzy", "write-token");
}

export function createFizzyCards(config = {}) {
  const base = (config.base ?? process.env.FIZZY_BASE ?? "https://fizzy.intern.deepwa7er.net").replace(/\/$/, "");
  const account = config.account ?? process.env.FIZZY_ACCOUNT ?? "1";
  const tokenFile = config.tokenFile ?? defaultTokenFile();

  async function token() {
    let raw;
    try {
      raw = await fs.readFile(tokenFile, "utf8");
    } catch (err) {
      throw new Error(`fizzy token unreadable (${tokenFile}): ${err.message}`);
    }
    const trimmed = raw.trim();
    if (trimmed === "") throw new Error(`fizzy token file is empty: ${tokenFile}`);
    return trimmed;
  }

  return {
    // POST /{account}/cards/{number}/comments.json. The body is ActionText
    // rich text — pass HTML. A 403 means the card is a draft (drafts are not
    // commentable); closed cards still accept comments, which is what lets
    // approve write to a card whatever its standing.
    async commentOnCard(number, html) {
      if (!Number.isInteger(number) || number < 1) throw new Error(`invalid card number: ${number}`);
      if (typeof html !== "string" || html.trim() === "") throw new Error("comment requires a body");
      const response = await fetch(`${base}/${account}/cards/${number}/comments.json`, {
        method: "POST",
        redirect: "manual",
        signal: AbortSignal.timeout(TIMEOUT_MS),
        headers: {
          Authorization: `Bearer ${await token()}`,
          "Content-Type": "application/json",
          Accept: "application/json",
        },
        body: JSON.stringify({ comment: { body: html } }),
      });
      if (response.status >= 300 && response.status < 400) {
        throw new Error("fizzy redirected the request — token rejected, or the account prefix is wrong");
      }
      if (response.status === 403) {
        throw new Error(`card #${number} is a draft and cannot take comments`);
      }
      if (response.status !== 201) {
        throw new Error(`fizzy comment on #${number} failed: HTTP ${response.status}`);
      }
      return response.json();
    },
  };
}
