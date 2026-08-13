# Pre-ship checklist — DW-001

Extracted from `docs/deepwater-style-guide.md` §10. Check before shipping any UI.

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

Source: DW-001 §1–§8. Re-check `docs/deepwater-style-guide.md` for the authoritative six rules and tokens if any item is unclear.
