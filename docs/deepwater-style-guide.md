# DEEPWATER STYLE GUIDE

```
┌──────────────────────────────────────────────────────────────────────┐
│ DOC. NO.  DW-001          REV. A          CLASSIFICATION: INTERNAL     │
│ SUBJECT   Warm-paper visual system for the personal Rails apps         │
│ ORIGIN    fleet/notes — application.css. Extended by readout.          │
│ COMPOSITE Warm-paper base + Bavarian blue + Gothic composite (Rev A)   │
│ IN USE    fleet/notes · agent-harness · readout                        │
└──────────────────────────────────────────────────────────────────────┘
```

Cream paper, warm ink, one Bavarian blue. Hierarchy comes from whitespace and
typography; the only things with depth are the things you can press or type
into. Data is set as instrumentation — uppercase, letterspaced, tabular — so a
page reads as a document with a few dials on it, not as a dashboard.

The six rules in §1 are the whole system. Everything after them is either a
consequence of a rule or an extension of one, and every extension names the
rule it derives from. That naming discipline is the point: it is what keeps
three separate apps looking like one hand made them.

Rev A adds five Gothic elements to the composite — §11. They were prototyped
on a living specimen (`deepwater-style-guide.html`), and the ones that survived
contact with rules 1–8 are now part of the system; the ones that did not are
rejected, with reasons, in §11. Every element names the rule it extends.

**Relationship to DG-001 / DG-002.** Retired and archived: the U.S. Graphics
design guide (DG-001) and TRITIUM (DG-002) now live at
`secondbrain/resources/archive/design-guide.md`. DW-001 is the fleet's live
composite guide.
---

## 1. THE SIX RULES

Canonical, carried verbatim from the notes app.

1. **Whitespace separates content.** No borders or dividers between items,
   sections, or text. Proximity and typography carry the hierarchy.
2. **Depth marks interactivity.** Solid offset shadows and outlines appear only
   on things you can press (buttons) or type into (fields).
3. **Warm light mode** (cream paper, warm ink); **charcoal dark mode**. Never
   pure neutral gray, never brown.
4. **One accent color** — a deep Bavarian blue — used only for interactive
   elements.
5. **Metadata is instrumentation:** uppercase, letterspaced, tabular numerals.
   Dashboard readouts, not prose.
6. **Motion is engineered:** short durations, strong ease-out, like a
   well-damped switch.

---

## 2. COLOR

```css
:root {
    color-scheme: light dark;   /* built-in controls follow the scheme */

    --bg:              #f7f2e9;  /* warm cream paper */
    --fill:            #fffdf8;  /* lighter surface: fields, logs, callouts */
    --text:            #1f1a12;  /* warm near-black ink */
    --text-muted:      #7a7264;
    --accent:          #0066b1;  /* Bavarian blue */
    --accent-contrast: #ffffff;
    --danger:          #dc2626;
    --good:            #2f6f3e;
    --ink:             #1f1a12;  /* button outlines and hard shadows */
}

@media (prefers-color-scheme: dark) {
    :root {
        --bg:              #121316;  /* charcoal, blue undertone */
        --fill:            #1c1e22;  /* same family, one step lighter */
        --text:            #eceef1;  /* neutral paper-white */
        --text-muted:      #8b9199;  /* cool gray */
        --accent:          #4d9de0;  /* dashboard backlight blue */
        --accent-contrast: #141311;
        --danger:          #f87171;
        --good:            #5cb974;
        --ink:             #000000;
    }
}
```

**Rules.**

1. **The accent is reserved for interaction** (rule 4). Links, primary buttons,
   focus rings, hover states. A chart line, a heading, or a highlighted figure
   never gets it — if it's blue, you can click it.
2. **`--ink` is not `--text`.** They happen to share a value in light mode, but
   `--ink` means "the hard edge and shadow of a physical key" and goes black in
   dark mode while `--text` goes paper-white. Never substitute one for the other.
3. **`--danger` marks a fact, not a mood** — a failed state, a destructive
   action, a figure that means something bad, a reference threshold on a chart.
   Never decoration, never "important but fine".
4. **`--good` is a wash, not a signal.** It exists for the delivered/needed gap
   behind data and is deliberately muted so it cannot compete with the ink. If
   something is merely fine, it stays `--text`.
5. **`--fill` is how a surface becomes distinct** without a border (rule 1).
   Fields, log panes, and callouts sit on it. It is the only permitted
   background change.
6. **The warmth flips.** Light mode is warm all the way through — the paper is
   cream and the ink is warm near-black. Dark mode is *not* the warm palette
   inverted: it is cool charcoal with a blue undertone and a neutral
   paper-white ink. Rule 3's "barely-warm charcoal" describes an earlier
   revision; the shipped dark palette is cool by design, so that the backlight
   blue reads as backlight.

**Measured contrast** (ratio against `--bg` / against `--fill`):

| Token          | Light        | Dark        |
| -------------- | ------------ | ----------- |
| `--text`       | 15.50 / 17.01 | 15.98 / 14.36 |
| `--text-muted` | **4.26** / **4.68** | 5.85 / 5.25 |
| `--accent`     | 5.33 / 5.84  | 6.38 / 5.73 |
| `--danger`     | **4.33** / **4.75** | 6.72 / 6.03 |
| `--good`       | 5.44 / 5.96  | 7.65 / 6.87 |

`--accent-contrast` on `--accent`: 5.94 light, 6.38 dark.

Two light-mode values fall under WCAG AA for normal text (4.5:1):
`--text-muted` on `--bg` at 4.26 and `--danger` on `--bg` at 4.33. Both carry
real content at 0.7rem, which is normal-size text — this is a known gap, not a
license. Darkening `--text-muted` toward `#6f675a` (≈4.9) and `--danger` toward
`#c81e1e` (≈5.0) clears it without changing the character of the palette.

---

## 3. TYPOGRAPHY

Four voices, and only four.

**Prose** — `system-ui, sans-serif` at `1rem`/`1.6`. Body text, findings,
descriptions. Set at a comfortable measure (§4). No uppercase, no letterspacing.

**Headings** — `ui-serif, Georgia, serif`. The serif is what makes the page a
document instead of an app chrome.

| Level        | Size      | Weight | Tracking  | Margin              |
| ------------ | --------- | ------ | --------- | ------------------- |
| `h1`         | `2rem`    | —      | `-0.02em` | `0`                 |
| `h2`         | `1.35rem` | 600    | `-0.01em` | `2.5rem 0 0.75rem`  |
| callout lead | `1.1rem`  | 600    | —         | contextual          |

Tighten tracking as size grows; large type set at default tracking always
looks loose.

**Instrumentation** (rule 5) — the readout voice. One recipe, applied
identically everywhere:

```css
font-size: 0.7rem;
font-weight: 600;
letter-spacing: 0.08em;
text-transform: uppercase;
font-variant-numeric: tabular-nums;
color: var(--text-muted);
```

It marks column headers, timestamps, units, axis labels, key terms, and
settings labels. It does **not** mark prose. If a human wrote the sentence for
another human to read, it is prose — findings and suggestions stay lowercase
and unletterspaced even though they sit beside instrumentation.

**Monospace** — `ui-monospace, SFMono-Regular, Menlo, monospace`, for program
output, diffs, and inline literals only. Monospace means "a machine produced
this and its alignment carries meaning." Prefer a `--mono` token over repeating
the stack.

**Tabular numerals are mandatory** anywhere numbers stack: tables, key rows,
settings values, tick labels. `font-variant-numeric: tabular-nums`.

**Masthead (G11)** — `UnifrakturCook, UnifrakturMaguntia, "Old English Text
MT", ui-serif, serif`. A blackletter display voice for the app or run title
alone. Nothing else gets it: body stays system-ui, headings stay Georgia, and
Fraktur is nearly unreadable at length, so the discipline is self-enforcing.
It degrades to the serif if the font cannot load.

---

## 4. LAYOUT & MEASURE

```css
.container { max-width: 64rem; margin: 0 auto; padding: 1.5rem 1.25rem; }
.measure   { max-width: 40rem; }
```

**40rem is the prose measure.** It is the notes app's container width and the
right measure for reading. Keep prose sections inside it.

**Widen the container only for data that genuinely does not fit**, and say why
in a comment. A ten-column results table cannot honestly be squeezed into 40rem
— widening the page is preferable to shrinking type below a readable size. When
you widen, the prose on that page keeps its measure via `.measure`; the page
does not get wider, the table does.

**The page never scrolls sideways.** Anything wide — a table, a log, a diff —
scrolls inside its own `overflow-x: auto` box.

Structure comes from vertical rhythm, not rules. The gap between two items *is*
the divider:

```css
.run          { padding: 0.75rem 0; }
.run + .run   { margin-top: 1rem; }   /* the gap replaces the divider */
```

**Verticality is the architecture (G1).** The whitespace is not merely
functional — it is the nave, and it is generous. The masthead opens with a tall
empty rise (`padding-top: 6rem`); headings run at `line-height: 1.4`; major
sections are separated by `6rem` of open space; prose gaps run `1.25rem`. The
void does the hierarchy's work, and it must never be filled.

**The nave and the aisle (G2).** Where a page must carry a register of metadata
beside prose, split the layout into a nave and an aisle: prose and components
fill the nave; instrumentation — section numbers, the rule each section derives
from, key/value readouts — runs in a narrow rail, the aisle. The page is
organized in bays: each section is a bay, and each bay's number sits in the
aisle, aligned to its heading by sharing the void. Extends rules 1 and 5.

---

## 5. DEPTH — THE ONLY PLACE IT APPEARS

Rule 2 is the sharpest rule in the system, and the easiest to erode. Two
treatments exist, and nothing else on the page may borrow either.

**The button is a physical key.**

```css
.button, input[type="submit"] {
    display: inline-block;
    padding: 0.6rem 1.1rem;            /* ~44px: a proper touch target */
    font: inherit;                     /* controls do not inherit the page font */
    font-weight: 600;
    color: var(--accent-contrast);
    background: var(--accent);
    border: 2px solid var(--ink);
    border-radius: 10px;
    box-shadow: 3px 3px 0 var(--ink);  /* solid offset — never a soft shadow */
    cursor: pointer;
    text-decoration: none;
    transition: transform 0.15s cubic-bezier(0.2, 0.8, 0.2, 1),
                box-shadow 0.15s cubic-bezier(0.2, 0.8, 0.2, 1);
}

.button:hover  { filter: brightness(0.95); }

/* The press: the key slides down into its own shadow. */
.button:active {
    transform: translate(3px, 3px);
    box-shadow: 0 0 0 var(--ink);
    filter: none;
}
```

The shadow offset and the active translate must match, or the key does not
land. A small variant scales both together (`2px 2px` shadow, `translate(2px,
2px)`).

Variants recolor; they never restyle. `--secondary` and `--quiet` take
`--fill` with accent or muted text; `--danger` takes `--fill` with danger text.
The border, radius, and shadow are constant — that constancy is what says
"pressable".

**The field is a recess.** No border at all; the `--fill` surface and the
radius do the work, and focus draws the only outline on the page.

```css
input, textarea, select {
    width: 100%;
    padding: 0.75rem 1rem;
    font: inherit;
    color: var(--text);
    background: var(--fill);
    border: none;
    border-radius: 10px;
}

input:focus, textarea:focus, select:focus {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
}
```

Labels are persistent and above the field (`display: block; font-weight: 600`),
never placeholder-only.

`radius: 10px` is the system's one geometry constant. Buttons, fields, logs,
and callouts all use it; small variants may drop to 8px.

---

## 6. MOTION

`0.15s cubic-bezier(0.2, 0.8, 0.2, 1)` — damped, not springy. That is the
whole vocabulary. Motion exists to make the key press feel mechanical; it never
reveals content, never gates a click, never loops.

Honor the user's setting:

```css
@media (prefers-reduced-motion: reduce) {
    * { transition: none !important; }
}
```

---

## 7. COMPONENTS

### Lists (rule 1)

`list-style: none`, zero padding, `.item + .item { margin-top: 1rem }`. The
title is a `display: block` link in `--text` that turns `--accent` and
underlines on hover — the hover state is where rule 4 earns its keep, since the
link is not blue at rest.

### Tables (extension of rules 1 and 5)

No rules, no zebra striping — the gaps carry the grouping. Every column header
is instrumentation; every figure is tabular.

- `border-collapse: collapse`, `font-variant-numeric: tabular-nums`, wrapped in
  `.table-scroll { overflow-x: auto }`.
- Numbers right-aligned, the first (label) column left-aligned and `600`.
- Padding is asymmetric — `padding-left: 1rem` on every cell, `0` on the first
  — so the table's left edge lines up with the page text.
- `white-space: nowrap` on headers and cells; a wrapped figure stops being a
  figure.
- Units belong in the column header, not a footnote:
  `.unit { display: block; font-weight: 400; text-transform: none; opacity: 0.75 }`.
- Color a figure only where it means something: `.figure--bad` → `--danger`,
  `.figure--none` → `--text-muted`. Never the accent.

### The verdict block (extension of rule 1)

A conclusion the reader must not miss, separated by space and a shift to serif
at `1.1rem`/`600` — not by a box, a border, or a background.

### Findings (extension of rule 1)

The plain-language reading of a run: serif title at `1.05rem`, prose body at
the normal measure. This is prose, so it does **not** get the instrumentation
treatment even though it sits among readouts. Suggestions are a plain `ul` with
`1.1rem` of indent and `0.6rem` between items.

### The reading key (extension of rules 1 and 5)

A two-column `dl` (`grid-template-columns: 7.5rem 1fr`) mapping a band to its
meaning. Rows are separated by space, never rules. The band the run actually
landed in is marked by promoting both `dt` and `dd` from `--text-muted` to
`--text` and bolding the `dd` — by weight and color, never by a colored box.

### Settings grid

`repeat(auto-fill, minmax(11rem, 1fr))`, instrumentation `dt`, tabular `dd`
with `overflow-wrap: anywhere`. `min-width: 0` on the cells so long values
shrink instead of blowing out the grid.

### Log / program output (extension of rule 1)

Monospace on `--fill` at `radius: 10px`, `0.8rem`/`1.5`, capped height with
`overflow: auto`, `white-space: pre-wrap`, `overflow-wrap: anywhere`. The fill
surface marks it as a distinct plane without needing a border.

### Flash messages and back links

Flashes are bold text in flow — `--danger` for alerts — with no box. Back links
are accent, `600`, underlined on hover only.

### The illuminated initial (G5, extension of rule 1)

One drop cap per page, opening the title — the incipit. A large first letter on
a `--fill` panel, floated so the title wraps beside it; set in the masthead
voice (§3) when a page has one, otherwise the serif. Ink, never accent. This is
the page's one ornament, and it is not a control: nothing else on the page may
borrow a drop cap.

### The 404 page (G10, extension of rule 1)

A page that has nothing to say may say it with a gargoyle. The 404 is the one
allowed moment of playfulness — a stone grotesque drawn in muted ink, the only
ornament on the page, over a single honest line and a back link. Comedy needs a
straight man; everywhere else the system stays entirely straight. Specimen:
`deepwater-404.html`.

### Charts

A chart is not interactive, so **it does not get the accent** (rule 4). The
plotted line is `--text`; the area wash is `--text` at `0.07` opacity;
`--danger` marks a reference threshold only, dashed. Tick labels and axis
labels use the instrumentation recipe in SVG units (`fill` rather than
`color`).

Two implementation notes worth keeping — both are real failure modes, not
preferences:

- **Inline SVG charts need an explicit height.** These SVGs carry
  `preserveAspectRatio="none"`, so they have no intrinsic aspect ratio for
  `height: auto` to resolve against and WebKit collapses them to zero. Set a
  fixed `height` (or an explicit `aspect-ratio`); a fixed height also stops the
  page reflowing on every poll.
- **A Chart.js canvas must be sized in CSS.** With
  `maintainAspectRatio: false` the chart fills its container, so the container
  needs a height or it collapses. And the canvas itself needs
  `width: 100%; height: 100%` — otherwise its layout size comes from its
  `width`/`height` attributes, which the controller sets to
  (measured size × devicePixelRatio) for a sharp backing store, so on a retina
  screen the element doubles every time it is measured.

---

## 8. ADDING TO THE SYSTEM

The style has survived three apps because extensions are made one way:

1. **Name the rule you are extending.** In a comment, in the stylesheet, next
   to the code. "Extension of rule 1" is the price of admission.
2. **Say what the existing system lacked.** The notes app had a color for "bad"
   and none for "good" because it never needed one; the delivery chart shades a
   signed gap, so `--good` was added — muted, so it stays a wash behind data.
3. **When a rule genuinely strains, resolve it — do not except it.** Two worked
   examples from agent-harness:
   - *The transcript.* Rule 1 forbids dividers, which is easy across a dozen
     notes and hard across four hundred agent events. The rhythm comes from
     rule 5 instead: agent prose is body text with room around it, everything
     mechanical is instrumentation. You see the prose when scanning and get the
     mechanics when reading. No line is drawn anywhere.
   - *Diffs.* Rule 4 allows one accent, and a diff needs more than one signal.
     The resolution: color by line **role** — added, removed, hunk header —
     reusing the status colors that already exist. Three roles, no syntax
     highlighter, no second palette to maintain.
4. **Never reach for a border.** If two things need separating, they need more
   space or a different type treatment. A border is always the wrong answer
   here, and it is always the first idea.

---

## 9. KNOWN DIVERGENCES

The three apps have drifted. Recorded honestly so the next person to touch them
can converge rather than pick one at random.

| | notes | agent-harness | readout |
| --- | --- | --- | --- |
| `.container` max-width | `40rem` | `46rem` | `64rem` + `.measure` |
| "good" token | — | `--success` `#15803d`/`#4ade80` | `--good` `#2f6f3e`/`#5cb974` |
| `--mono` token | — | yes | inlined |
| `prefers-reduced-motion` | no | yes | no |
| Heading scale | `h1` only | `h1` + `.section-heading` (`1.1rem`/500) | `h1` + `h2` (`1.35rem`/600) |

To converge: pick one name and one pair of values for the positive color;
define `--mono` everywhere; add the reduced-motion block to notes and readout;
and treat 40rem as the prose measure with per-app container widths justified in
a comment.

The Gothic composite (§11) is defined at guide level and demonstrated on the
specimen pages; it has not been carried into the three apps yet. Carry it into
one app first, per §8, and record the divergence here as it ships.

---

## 10. PRE-SHIP CHECKLIST

```
[ ] Zero borders and dividers between content. Is every separation a gap?
[ ] Does anything but a button or a field have a shadow or an outline?
[ ] Is the accent used anywhere that is not interactive?
[ ] Do the shadow offset and the :active translate match?
[ ] Are all stacked numbers tabular? Are units in the column header?
[ ] Is every uppercase letterspaced run actually metadata, not prose?
[ ] Is prose inside a 40rem measure?
[ ] Does any wide element scroll the page sideways instead of itself?
[ ] Does every color come from a token — no literal hex outside :root?
[ ] Is dark mode cool and light mode warm — no brown, no neutral gray?
[ ] Does every new class name the rule it extends?
[ ] Do transitions honor prefers-reduced-motion?
[ ] Is the page's vertical rhythm a nave — tall voids, never filled?
[ ] If a page uses an aisle, does only instrumentation run in it?
[ ] Is there at most one ornament — one initial, one gargoyle — per page?
[ ] Is blackletter used on the masthead only?
```

---

## 11. THE GOTHIC COMPOSITE — REV A

Rev A tries eleven Gothic ideas on a specimen page and keeps five. What
survived is not ornament: Gothic's translatable DNA for this system is
verticality, structure, and *one* sacred thing per page.

**Kept.**

| Idea | Element | Extends |
| --- | --- | --- |
| G1 | The nave is whitespace — tall voids carry the hierarchy | rule 1 |
| G2 | Nave and aisle — instrumentation runs in a bay rail | rules 1 & 5 |
| G5 | The illuminated initial — one drop cap per page | rule 1 |
| G10 | The gargoyle — the 404 page's one allowed joke | rule 1 |
| G11 | The blackletter masthead — the title only | display voice |

Each lives where it belongs: G1 and G2 in §4, G5 and G10 in §7, G11 in §3.

**Tested and rejected**, with the reason recorded so nobody re-tries them:

| Idea | Why not |
| --- | --- |
| G3 | The vault of the document — pure metaphor, no behavior |
| G4 | The pointed arch — a second geometry strained the depth rule; the verdict was already the altar |
| G6 | The quatrefoil divider — clutter; the pure void is stronger |
| G7 | Chartres blue framing + `--stone` — the framing changed nothing; the stone broke "one surface" |
| G8 | Stone grain — the flat-paper rule |
| G9 | Rubrication — rule 4: color means something, or it means nothing |

**Compounding.** Extensions may delegate responsibility to one another, and the
record must say so. The 6rem section gap was introduced as an h2 margin under
G1, then carried by a divider element under G6, then returned to the h2 margin
when G6 was rejected. A later editor reading the stylesheet sees only the
margin; this note is why it exists.

**Specimens.** `deepwater-style-guide.html` (G1, G2, G5, G11) and
`deepwater-404.html` (G10) are the reference implementations.

---

*Origin: `fleet/notes/app/assets/stylesheets/application.css`. Extended by
`readout` (tables, charts, verdicts, keys), `agent-harness` (transcript, diffs,
status), and Rev A's Gothic composite (§11). Specimens:
`deepwater-style-guide.html`, `deepwater-404.html`. When in doubt: more space,
less ink, one blue, one gargoyle.*

```
END DW-001 · REV A
```
