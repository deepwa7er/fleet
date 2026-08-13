# DW-001 Appendix

Historical and negative guidance extracted from `docs/deepwater-style-guide.md` so the enforceable core (§1–§8, §11 kept) stays scannable. The `taste` skill is the fleet-wide negative filter — this appendix preserves the *reasons* so nobody re-tries rejected ideas.

## Known divergences (pre-monorepo)

Preserved from DW-001 §9 at `3169da3` (2026-08-13). At that time the three apps had drifted:

| | notes | agent-harness | readout |
| --- | --- | --- | --- |
| `.container` max-width | `40rem` | `46rem` | `64rem` + `.measure` |
| "good" token | — | `--success` `#15803d`/`#4ade80` | `--good` `#2f6f3e`/`#5cb974` |
| `--mono` token | — | yes | inlined |
| `prefers-reduced-motion` | no | yes | no |
| Heading scale | `h1` only | `h1` + `.section-heading` (`1.1rem`/500) | `h1` + `h2` (`1.35rem`/600) |

To converge: pick one name and one pair of values for the positive color; define `--mono` everywhere; add the reduced-motion block to notes and readout; and treat 40rem as the prose measure with per-app container widths justified in a comment. The single Cargo workspace (`fleet/Cargo.toml:8-30`) does not by itself converge the Rails stylesheets — converge per §8 when touching.

### Gothic shipping (at extraction)

The Gothic composite (§11) had shipped in two apps beyond the specimen pages: the public_site (G1, G5, G11 on the home page; G10 on the 404) and the blog (the same, with G5/G11 carried onto each post's title — the post title is the incipit of its page, so a post title is set in Fraktur). Fraktur on a title is the one accepted strain on G11: it holds because a title is display text, not prose, and the `26rem` measure bounds the run. Within the original three apps the composite was still not carried; carry it into one of them per §8 and record the divergence here.

---

## Rejected Gothic ideas

Preserved from DW-001 §11 at `3169da3`. Eleven Gothic ideas were tried on `deepwater-style-guide.html`; five were kept (G1, G2, G5, G10, G11 — see §11 and §3/§4/§7) and six were rejected with reasons so nobody re-tries them.

| Idea | Why not |
| --- | --- |
| G3 | The vault of the document — pure metaphor, no behavior |
| G4 | The pointed arch — a second geometry strained the depth rule; the verdict was already the altar |
| G6 | The quatrefoil divider — clutter; the pure void is stronger |
| G7 | Chartres blue framing + `--stone` — the framing changed nothing; the stone broke "one surface" |
| G8 | Stone grain — the flat-paper rule |
| G9 | Rubrication — rule 4: color means something, or it means nothing |

See also `taste` — the fleet-wide anti-slop filter that covers glassmorphism, purple gradients, `rounded-2xl` everywhere, etc. Where `taste` and DW-001 conflict, DW-001 overrides (fleet paper `#f7f2e9` is intentional, not taste's `#faf8f4` ban — see `AGENTS.md` and `docs/deepwater-style-guide.md:42`).

### Compounding note

Extensions may delegate responsibility to one another, and the record must say so. The 6rem section gap was introduced as an h2 margin under G1, then carried by a divider element under G6, then returned to the h2 margin when G6 was rejected. A later editor reading the stylesheet sees only the margin; this note is why it exists.

*Preserved at 3169da3 per card #37. For the live spec, see `docs/deepwater-style-guide.md` and specimens `deepwater-style-guide.html`, `deepwater-404.html`.*
