# Readout

A Rails dashboard for [campfire-stress](../campfire-stress) load-test results.

It reads the `results/<timestamp>/` directories the harness writes, and answers
the question a raw k6 summary cannot: **at what point did it stop keeping up,
and what ran out?**

## Why it exists

k6's end-of-run summary aggregates every ramp step into one set of percentiles.
A p95 spanning 25 and 800 virtual users describes neither, and the knee — the
level where the application stops keeping up — is averaged away entirely.

Readout re-buckets every sample by the VU count in effect when it was recorded,
then joins that against the server-side CPU trace, so a latency knee can be
attributed rather than guessed at.

## Running it

```sh
bin/readout
```

That prepares the database, imports any results, and serves on
<http://localhost:3000>. Options:

| | |
|---|---|
| `--no-import` | serve without importing (use during a load run) |
| `--import-only` | import and exit, without a server |
| `-p 3123` | serve on another port |
| `--open` | open a browser once the port answers |

**It refuses to import while `k6` is running.** Parsing a 700k-line CSV is
several seconds of hard CPU, and this machine is the load generator — stealing
cores mid-run shows up as latency that would be wrongly blamed on the
application under test.

The plain Rails commands work too, since `mise.toml` pins Ruby 3.4.8 for this
directory:

```sh
bin/rails db:prepare && bin/rails server
bin/rails runner 'Analysis::Importer.import_all(Rails.configuration.x.campfire_stress.results_root)'
```

The results directory defaults to `~/code/campfire-stress/results` and is
overridable with `CAMPFIRE_STRESS_RESULTS`. You can also press **Rescan
results** on the index.

Re-importing a directory replaces its derived rows in place, so it is the normal
way to refresh a run after changing the analyzer.

## Launching tests from the UI

There is **one** Readout, at <https://readout.intern.deepwa7er.net>, and it can
do everything: browse results, configure and launch a load test, watch k6's
output live, and receive the results when the run finishes.

The dashboard runs on the VPS and drives a **runner** service on whichever
machine is generating the load — where k6 lives and where the CPU to generate
load is.

```
                        ┌─► runner (MacBook)  ──┐
browser ─► readout (VPS)┤                       ├─► k6 ─► Campfire (fedora-1)
                  ▲     └─► runner (Desktop) ──┘             │
                  └──────── bin/publish posts results ◄───────┘
```

### Which machine generates the load

The launch form asks, and only lists machines whose runner is answering — a box
that is asleep simply is not on the list.

| | |
|---|---|
| **MacBook** | the dev box, on **Wi-Fi** |
| **Fedora desktop** | 16 cores, **wired** to the same LAN as the Campfire host |

This is not a convenience. A room page is ~400KB, so the wireless link can
saturate before the application does — and telling those two apart is most of
what this harness exists for. Running the same test from the wire is the
cheapest way to rule the network out. The desktop is also a much larger machine,
so it is far less likely to be the thing that runs out first.

Every run records which box produced it (`bin/run.sh` writes `machine=` into
`run-config.txt`), and the run page and index both say so. Runs from before this
existed say **generator not recorded** rather than being attributed to whichever
machine seems likely.

**One run at a time, across all machines.** Each runner refuses a second run
within its own process, but they cannot see each other — and every generator
points at the same Campfire, so a run on one machine would measure the load the
other is generating. The dashboard is the only thing that can see both, so it
enforces this.

Setup is one command per machine:

```sh
cd ~/code/campfire-stress && deploy/install-runner-agent.sh   # on the Mac
scp deploy/provision-desktop.sh desktop:/tmp/ && ssh desktop 'bash /tmp/provision-desktop.sh'
```

The form has **two levers**, both pointing the same way — turn either up and the
server does more work:

| | |
|---|---|
| **Load** | concurrent users (requests/sec for `arrival`) |
| **Minutes** | how long to hold it |

They map to whichever variable is the natural intensity dial for the chosen
scenario, so you never have to remember which is which:

| scenario | Load becomes | Minutes becomes |
|---|---|---|
| `browsing` | `VUS` | `HOLD` |
| `ramp` | `RAMP_STEPS` — quarter, half, full | `STEP_HOLD` |
| `arrival` | `ARRIVAL_RATE` | `ARRIVAL_DURATION` |
| `smoke` | — | — |

`USER_POOL` is sized to the load automatically rather than exposed: with fewer
identities than VUs they start sharing sessions, which distorts presence and
unread bookkeeping. That is a property of an honest run, not a preference.

The harness's other ~20 levers — room size, action mix, think time, subscriber
count — are hidden here, not removed. They remain available through the runner
API and the CLI.

Start it and the run page shows two things: a **Keeping up?** chart of p95
response time while the run is in flight, with current server CPU beside it, and
the streaming harness output below.

Latency is the honest answer to "is the server handling this" — flat means it is
keeping up, rising means it is not. CPU sits next to it as a number rather than a
second line, because it answers a different question (*why*): latency climbing
while CPU stays low is not a compute limit.

The live chart is **Chart.js on a canvas**, and it sits deliberately *outside* the
polled Turbo Frame. The frame replaces its contents every two seconds; a chart
rebuilt that often flickers and, with `height: auto`, can collapse to nothing in
WebKit. The canvas is created once and only its data changes.

The static charts on an imported run stay server-rendered SVG — see
`app/helpers/chart_helper.rb` for why the two cases differ.

Progress covers the **whole run**, accumulated incrementally by the runner
(reading only bytes appended since the last poll), so watching a test cannot slow
down the machine generating it.
When it finishes the runner publishes automatically, so the run appears on the
dashboard with no further action. **Publish results** on the run page is the
retry if that fails.

### What this exposes

A runner binds its machine's **tailnet IPv4**, not loopback — that is what lets
the VPS drive it, and it is the price of having one app instead of two.

So a service that starts processes on those boxes is reachable from every device
on the tailnet. It is much narrower than an arbitrary command endpoint (fixed
command, allowlisted scenarios, bounded levers, shared token — see
`campfire-stress/runner/README.md`), but it is the same category of exposure that
`breakwater.toml` flags for the `harness` route. Worth revisiting if the tailnet
ever gains a device trusted less than these machines.

A consequence worth knowing: **a machine must be awake**. If it is asleep or off
the tailnet its name does not resolve, it is left out of `RUNNERS`, and it simply
does not appear on the form. Reading results keeps working — that degradation is
deliberate, and with the desktop usually powered off it is the normal case rather
than an edge one.

Addresses are resolved on the VPS host at service start, because a container
there cannot resolve MagicDNS. Tailnet IPs are not constants, so a runner also
reports its own name in `/healthz`: a machine answering under another machine's
name is left off the form rather than launched into, since that would record a
run on one box while the form claimed another.

## A known error you can ignore

The new-test page sometimes shows **"The deployed server could not be read —
runner unreachable: Net::ReadTimeout"** where the variant switcher belongs.

Reading which build of Campfire is deployed is a live round trip on every page
load: readout, to the runner on the dev box, to `ssh laptop`, to
`docker inspect`, and back, against a five second budget. It normally takes
about 400ms. Occasionally it does not — one measured request took 10.2 seconds,
and the one a minute later took 0.6. It is likeliest mid-run, when the dev box
is generating load and the machine under test is under it.

**We are not fixing this. Refresh the page until the round trip lands inside
five seconds.**

That is a considered choice, not neglect. Nothing about a run depends on this
call: the variant recorded against a run is probed by `bin/run.sh` at run start
and written into its `run-config.txt`, so runs stay correctly labelled while
this frame is showing an error. And the frame fails loudly rather than showing
a stale or invented variant, which is the property worth keeping. The cost is
that the switch buttons are missing until it loads cleanly, so changing variant
needs a reload first.

The fix, if it ever becomes worth it, is to cache the reading in the runner for
a few seconds and invalidate it after a switch — which takes the ssh off the
page-load path without going back to remembering what was last deployed.

## Deployment

Live at **<https://readout.intern.deepwa7er.net>** (tailnet only, behind
breakwater's wildcard cert).

```sh
bin/publish            # parse this machine's results and post them up
tugboat deploy         # ship a new build of the app itself
```

Those are two different things, and the split is the whole design:

| | changes | how |
|---|---|---|
| the **app** | code | `tugboat deploy` — builds an amd64 image, ships the tar, restarts the unit |
| the **data** | new load-test runs | `bin/publish` — parses here, posts to `/api/runs` |

`bin/publish` exists because the raw results only live on the machine that
generated them. A run's directory is ~165MB of CSV and JSON; what travels is the
~600KB worth keeping (`Analysis::RunBundle`) — the parsed levels, the per-second
series and the chart payload. The dashboard upserts it by stamp.

It runs on **generator** machines, which are not the dashboard and need not be
Rails hosts, so it is stdlib-only Ruby: no bundler, no Rails boot, no database.
The analysis files are plain Ruby over IO for exactly this reason. Requiring them
directly is what keeps one implementation of the arithmetic rather than a second
one written in whatever language the generator happens to have.

> This replaced importing into a local production database and rsyncing that
> file up. That worked only while exactly one machine generated load: two
> machines each hold a database containing only their own runs, so whichever
> published last replaced the other's history — silently, since nothing about
> the result looked wrong. Per-run and idempotent is what makes a second
> generator possible at all.

The deployed instance has no results directory and presents itself as a
**published snapshot**: no path, no rescan button. It is not a live view of a
running test.

### How it runs on the VPS

readout is the fleet's first containerised deployable. The VPS has Ruby 3.2.3 and
this app needs 3.4.8, so a native install would mean maintaining a second Ruby on
the host; the image carries its own instead.

It still deploys like everything else. tugboat ships one artifact (the image tar)
and restarts one systemd unit, whose `ExecStartPre` loads the tar — so a failed
health check rolls back to the previous image exactly as it would to a previous
binary. `deploy/provision.sh` sets up the unit, and is a one-time/on-change step.

The container publishes to loopback `:8101`; breakwater fronts it. Its route in
`breakwater.toml` is hand-written, because readout's repo lives outside the fleet
monorepo and `tugboat fleet gen` cannot read this manifest (same arrangement as
lagoon).

Puma runs a single worker and the container is capped at 400MB — the VPS has 2GB
total shared with the rest of the fleet. It settles around 130MB.

> `config/master.key` is gitignored and must exist at `/opt/readout/master.key`
> on the host, owned by uid 1000 so the container's `rails` user can read it.
> Without it the app cannot boot.

## Where this runs, and where it must not

It runs in two places on purpose:

- **This Mac**, via `bin/readout` — where the results are, for working through a
  run right after it finishes.
- **The VPS**, via `tugboat deploy` — for reading results from a phone, or when
  the laptop is asleep.

**Never on `fedora-1`.** That box is the system under test, and a web server
sitting alongside Campfire becomes part of what you are measuring. The VPS is a
different machine entirely, so hosting there costs the measurements nothing —
only the data has to travel.

That the data travels well is not an accident: only the import path touches the
filesystem, so the SQLite file is a complete deliverable on its own.

## What it will and will not conclude

A run that degrades while the server still has CPU headroom is **not** reported
as an application limit. `fedora-1` is on Wi-Fi and a Campfire room page is
~400KB, so it is entirely possible to saturate the wireless link before the app
breaks a sweat — which looks identical to an application limit if you only read
client-side latency.

So the verdict distinguishes three cases:

| Evidence | Verdict |
|---|---|
| No level degraded | limit is above the load tested |
| Knee + CPU ≥ 85% of the host | compute is a fair explanation |
| Knee + CPU headroom | *not* compute — suspect the link or SQLite write contention |

That threshold is deliberately high (`Analysis::CPU_SATURATION_FRACTION`). A run
peaking at 70% of the box has real headroom, and calling that "CPU bound" would
pin the knee on the application when the ceiling was somewhere else.

Server-side figures are always bounded to the run's own time window. A
`server.csv` is not guaranteed to describe only its own run: until the harness's
sampler process handling was fixed, every `bin/run.sh` invocation leaked a remote
sampling loop that kept appending to that run's file — recording load generated
by *later* runs. Windowing makes the numbers correct regardless.

## Layout

```
app/models/analysis.rb                    shared constants and thresholds
app/models/analysis/metrics_file.rb       streams k6 metrics.csv (files reach ~700k lines)
app/models/analysis/level_breakdown.rb    segments the VU timeline; the core judgement
app/models/analysis/percentile.rb         linear-interpolated percentile, matching k6's
app/models/analysis/server_trace.rb       parses the server-side CPU/WAL sampler output
app/models/analysis/run_config.rb         reads run-config.txt
app/models/analysis/importer.rb           the only part that knows about ActiveRecord
app/helpers/chart_helper.rb               inline SVG charts, server-rendered
app/helpers/runs_helper.rb                figure formatting and the run verdict
app/models/harness/client.rb              talks to the local runner service
app/controllers/test_runs_controller.rb   configure, launch, watch, import
```

Parsing is plain Ruby over `IO`, deliberately separate from the models: the
largest `metrics.csv` seen so far is 695k lines, `CSV.parse` is avoided entirely,
and only the first three columns are read.

## Notes on the implementation

**Levels come from the `vus` gauge, not the configured schedule**, so the output
reflects what actually ran.

**Segments must be contiguous.** A ramp visits the same level twice, once
climbing and once on the way down. Treating those as one window merges two
unrelated periods and counts the gap between them as elapsed time, which
silently deflates throughput. There is a test for this.

**A settling quarter is trimmed** from the front of each level: a level just
stepped into is still filling connection pools and warming caches.

**A plateau with no traffic in it is not a level.** Every VU-based scenario ends
with `gracefulRampDown` and `gracefulStop`, so a few stragglers finishing their
last session hold a constant two or three VUs for longer than the minimum hold —
long enough to look exactly like a level. Counted as one, it became a point at
(2 people, 0 req/s) that the chart drew a line up from, turning the dying tail of
a run into an apparent load curve. A level is dropped when nothing completed
during it; a level whose requests all failed still counts, since k6 records those
too.

**A single-level run is charted against time, not against load.** `chat` and
`enterprise` hold one population from start to finish, so throughput against
people is one dot and nothing else — and a run cancelled before its level was
held long enough had no points at all. Those runs are drawn against elapsed time
instead, from a per-second series recorded at import (`ThroughputSample`). Only a
run that genuinely visited two or more levels gets the curve. Seconds in which
nothing completed are stored as zeros: unlike a latency mean, a rate really is
zero when nothing finished, and a gap would hide a stall.

**Charts are server-rendered SVG from Ruby**, not a JavaScript charting library.
The data is already on the server and the charts are static; a chart library
would mean a build step and a dependency for what amounts to a polyline. They
take their colors from CSS custom properties, so light and dark mode follow for
free.

## Styling

The design rules, tokens, and components come from the **notes** app
(`~/code/fleet/notes/app/assets/stylesheets/application.css`) and are carried
over verbatim at the top of `app/assets/stylesheets/application.css`.

Two things that app did not need are marked in the stylesheet as extensions,
with the rule each derives from:

- **Tables** — from rules 1 and 5: no row rules or zebra striping (gaps carry
  the grouping), every column header is instrumentation, all figures use tabular
  numerals.
- **A wider container** — the notes app uses a 40rem prose measure; a ten-column
  results table cannot honestly fit it, so the shell is 64rem and prose sections
  keep the original measure via `.measure`.

The accent stays reserved for interactive elements (rule 4), which is why chart
lines are drawn in ink rather than blue, and why a breached figure is colored
with `--danger` rather than the accent.

## Tests

```sh
bin/rails test
```

The analyzer is covered against hand-built timelines (segmentation, settling,
throughput) and end to end against a generated results directory, so the parsing
path is exercised without depending on an 80MB artifact.
