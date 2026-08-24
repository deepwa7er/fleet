# change

The shared DW-002/DW-003 source-control domain. `dw` and Skiff link this
crate directly; neither reaches through the legacy Node bridge to read or
author a change.

It owns:

- the typed change, round, annotation, landing, deploy, and export model;
- one fsync'd append-only JSONL log per change at the existing
  `~/.local/share/skiff-bridge/changes/<repo>/<card>.jsonl` location;
- cross-process read/validate/append locking, because `dw` and Skiff can
  author the same change from separate processes;
- repository-safe jj lookup and additive-round validation;
- git-format patch parsing into files, hunks, and old/new numbered lines;
- exact `(path, side, line)` annotation-anchor validation;
- the fetch/rebase/conflict-check/push landing boundary;
- token-gated fleet deploy triggering and durable job outcomes;
- exclusion-by-default public-record export and Fizzy landing comments;
- explicit tail recovery through `dw finish`, which skips completed steps.

The log remains compatible with records written by the Node bridge. Unknown
or torn event lines are ignored. New paired facts such as request+reopen and
landed+shipped are each one event, so a crash cannot persist half of the
state transition.

`cargo test -p change` includes real colocated-jj landing, bare-Git record,
and fake-daemon tail integrations. The jj tests skip only on a host where
`jj` is absent; the Fleet development machines are expected to have it
installed.
