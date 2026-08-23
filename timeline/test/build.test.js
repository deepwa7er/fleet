// timeline/test/build.test.js — the generator end to end over a fixture
// record: entry pages carry the annotated change (diff lines numbered,
// annotations at their anchors, stranded ones labelled), the index lists
// newest first, every interpolated string is escaped, and one corrupt
// entry never takes the timeline down.
import { before, after, describe, it } from "node:test";
import assert from "node:assert/strict";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { build, parseDiff, renderEntryPage } from "../build.mjs";

const DIFF = [
  "diff --git a/app/models/harness.rb b/app/models/harness.rb",
  "@@ -1,2 +1,3 @@",
  " class Harness",
  "+  def available_models = cache.fetch(:models) # <script>alert(1)</script>",
  " end",
  "",
].join("\n");

const ENTRY = {
  repo: "fleet",
  card: 81,
  title: "pi model picker <b>bold</b>",
  landedAt: "2026-08-23T12:00:00Z",
  tip: "57e14554a0bb99",
  rounds: [
    {
      n: 1,
      author: "agent",
      changeId: "k".repeat(32),
      commit: "abc123def456",
      gatesRan: [ "cargo test", "clippy" ],
      worthKnowing: [ "+1 dependency" ],
      diff: DIFF,
      annotations: [
        { path: "app/models/harness.rb", line: 2, side: "new", text: "cached because the phone re-polls" },
        { path: "elsewhere.rb", line: 9, side: "new", text: "stranded note" },
      ],
    },
  ],
  afterward: [],
};

describe("parseDiff", () => {
  it("numbers both sides", () => {
    const [ file ] = parseDiff(DIFF);
    assert.equal(file.newPath, "app/models/harness.rb");
    const lines = file.hunks[0].lines;
    assert.deepEqual(lines.map((l) => l.kind), [ "context", "add", "context" ]);
    assert.deepEqual(lines.map((l) => l.newNumber), [ 1, 2, 3 ]);
    assert.deepEqual(lines.map((l) => l.oldNumber), [ 1, null, 2 ]);
  });

  it("degrades on malformed input instead of raising", () => {
    assert.deepEqual(parseDiff(null), []);
    assert.deepEqual(parseDiff("just words"), []);
  });
});

describe("the built site", () => {
  let tmp;
  let out;

  before(async () => {
    tmp = await fs.mkdtemp(path.join(os.tmpdir(), "timeline-"));
    const recordDir = path.join(tmp, "record");
    out = path.join(tmp, "dist");
    await fs.mkdir(path.join(recordDir, "fleet"), { recursive: true });
    await fs.writeFile(path.join(recordDir, "fleet", "81.json"), JSON.stringify(ENTRY));
    await fs.writeFile(
      path.join(recordDir, "fleet", "80.json"),
      JSON.stringify({ ...ENTRY, card: 80, title: "older change", landedAt: "2026-08-20T09:00:00Z", rounds: [] })
    );
    await fs.writeFile(path.join(recordDir, "fleet", "99.json"), "{corrupt");
    const count = await build(recordDir, out);
    assert.equal(count, 2, "the corrupt entry is skipped, not fatal");
  });

  after(async () => {
    await fs.rm(tmp, { recursive: true, force: true });
  });

  it("indexes newest first with instrumentation lines", async () => {
    const index = await fs.readFile(path.join(out, "index.html"), "utf8");
    assert.match(index, /2 shipped changes/i);
    assert.ok(index.indexOf("fleet/81.html") < index.indexOf("fleet/80.html"), "newest first");
    assert.match(index, /fleet #81 · 1 round · 2026-08-23 · 57e14554a0bb/);
    assert.match(index, /pi model picker &lt;b&gt;bold&lt;\/b&gt;/, "titles are escaped");
  });

  it("renders the annotated change", async () => {
    const html = await fs.readFile(path.join(out, "fleet", "81.html"), "utf8");
    assert.match(html, /agent ran cargo test · clippy/i);
    assert.match(html, /worth knowing/i);
    assert.match(html, /diff-line--add/);
    assert.match(html, /&lt;script&gt;alert\(1\)&lt;\/script&gt;/, "diff text is escaped");
    assert.ok(!html.includes("<script>alert"), "no raw script survives");
    // The annotation sits directly after its anchored line.
    const anchor = html.indexOf("available_models");
    const annotation = html.indexOf("cached because the phone re-polls");
    assert.ok(anchor !== -1 && annotation > anchor);
    assert.match(html, /annotations whose lines this diff no longer shows/);
    assert.match(html, /elsewhere\.rb:9/);
  });

  it("renders a roundless entry without failing", () => {
    const html = renderEntryPage({ repo: "fleet", card: 80, title: null, landedAt: "2026-08-20T09:00:00Z", tip: "aa", rounds: [] });
    assert.match(html, /change #80/);
  });
});
