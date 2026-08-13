import { Controller } from "@hotwired/stimulus"

// The live throughput chart, drawn straight onto a canvas at 30fps.
//
// WHY NOT A CHART LIBRARY:
//
// Chart.js rebuilds scales, ticks and layout on every update(). That is fine
// once a second; at 30fps over a series growing to a couple of thousand points
// it falls behind and stutters. Drawing one polyline is about a hundred lines of
// canvas code and holds 30fps without effort, so the library was costing 186KB
// to do the job slower.
//
// WHY THERE IS A PLAYHEAD:
//
// Data arrives in batches — one fetch every few hundred milliseconds brings a
// dozen new points at once. Drawing them the moment they land makes the line
// leap forward and then sit still until the next batch, which reads as stuttering
// however smooth the curve through the points is. So the chart keeps a playhead
// that advances with wall-clock time and reveals points as it passes them,
// deliberately trailing the newest data by a small buffer. It is the same idea as
// a video player holding a fraction of a second in hand: the line then extends
// continuously at frame rate, and a late fetch costs smoothness rather than
// causing a visible jump.
//
// WHAT MAKES IT SMOOTH RATHER THAN JUST FAST:
//
// The data arrives already at 33ms resolution — each point counts requests over
// the preceding FULL SECOND, stepped forward 33ms at a time (see the runner's
// rolling.go). So there is real sub-second detail to animate and every point
// still means something. Fetching happens a few times a second; drawing happens
// every frame, so motion stays continuous between fetches.
//
// Both axes are fixed for the whole run — x to the known 50-second duration, y
// to the requested rate plus headroom — so the frame never resizes and the line
// simply grows into it.
export default class extends Controller {
  static targets = ["canvas", "warning", "readout"]
  static values = {
    url: String,
    // "deliveries", "latency" or "requests". One controller, three charts: they
    // differ only in which series they read, what the reference line means, and
    // which side of it counts as good.
    series: { type: String, default: "deliveries" },
    // Milliseconds under which a message still feels immediate. Above this the
    // conversation stops feeling live.
    latencyGoal: { type: Number, default: 1000 },
    // Whether the series is still being written. A finished run's payload is
    // stored and never changes again, so its page fetches once and stops;
    // polling it would be a request every couple of seconds, for the same
    // bytes, for as long as the tab stayed open.
    live: { type: Boolean, default: true },
    // Fetching faster buys nothing: it is a round trip to another machine, and
    // the series it returns is already dense.
    fetchInterval: { type: Number, default: 400 }
  }

  connect() {
    this.points = []
    this.target = []
    this.running = true

    // Seconds of data deliberately held back, so there is always something
    // ahead of the playhead to reveal. Must exceed the fetch interval or the
    // playhead runs dry between batches and stalls -- the very stutter this
    // exists to remove.
    this.buffer = (this.fetchIntervalValue / 1000) * 1.75
    this.playhead = null
    this.lastFrameAt = performance.now()
    this.reportedError = false

    this.resize = this.resize.bind(this)
    window.addEventListener("resize", this.resize)
    this.resize()

    this.fetchData()
    this.frame = requestAnimationFrame((now) => this.tick(now))
  }

  disconnect() {
    cancelAnimationFrame(this.frame)
    clearTimeout(this.fetchTimer)
    window.removeEventListener("resize", this.resize)
  }

  async fetchData() {
    let loaded = false

    try {
      const response = await fetch(this.urlValue, { headers: { Accept: "application/json" }, cache: "no-store" })
      if (response.ok) {
        loaded = true
        const payload = await response.json()
        if (this.seriesValue === "latency") {
          this.points = payload.delivery_latency || []
          // A flat goal rather than measured demand: for latency there is no
          // "owed" figure, only a threshold past which a room stops feeling live.
          this.target = this.points.map((p) => ({ t: p.t, v: this.latencyGoalValue }))
        } else if (this.seriesValue === "requests") {
          // Requests the server finished, whatever the scenario. No reference
          // line: nobody asked for a particular number of them, so there is no
          // "owed" to shade against — unlike deliveries, which are owed by
          // arithmetic the moment somebody posts.
          this.points = payload.rps || []
          this.target = []
        } else {
          // A chat run measures deliveries; a request run measures requests.
          this.points = (payload.deliveries || []).length ? payload.deliveries : (payload.rps || [])
          this.target = payload.target_series || []
        }
        this.running = payload.running !== false
        this.showWarning(payload.dropped || 0)

        // Both axes are fixed for the whole run, from numbers known before it
        // starts: the target rate and the scenario's fixed shape, which the
        // runner reports as duration_s. A frame that rescales as data arrives
        // makes a steady line look like it is moving, and makes runs impossible
        // to compare by eye.
        this.setBounds(payload)
      }
    } catch {
      // A dropped poll is not worth blanking the chart over; the next one will
      // almost certainly succeed.
    }

    // One successful fetch is everything a finished run's page needs. A failed
    // one still retries: giving up on a dropped request would leave the chart
    // sitting on "waiting for the first second of data" for good.
    if (loaded && !this.liveValue) return

    // Retries on a page that is not live are paced gently — something is wrong
    // with the request, and asking harder will not fix it.
    const delay = this.liveValue && this.running ? this.fetchIntervalValue : 2000
    this.fetchTimer = setTimeout(() => this.fetchData(), delay)
  }

  setBounds(payload) {
    if (this.xMax === undefined) {
      // A little past the end, so the final points are not welded to the edge.
      this.xMax = (payload.duration_s || 50) * 1.06
    }
    if (this.seriesValue === "latency") {
      // Start at twice the goal so an instant room does not fill the frame, and
      // let it grow from there if things get slow.
      if (this.yMax === undefined) this.yMax = this.latencyGoalValue * 2
    } else if (this.yMax === undefined && payload.target_rps > 0) {
      // Headroom above the target: the achieved line should have somewhere to
      // go, and a line that touches the ceiling reads as clipped.
      this.yMax = this.niceCeiling(payload.target_rps * 1.2)
    }

    // Only ever grow. A run that overshoots must stay on screen, but the frame
    // must never shrink back or it starts breathing again.
    const latest = this.points.length ? this.points[this.points.length - 1].t : 0
    if (latest > this.xMax) this.xMax = latest * 1.06

    const peak = Math.max(0, ...this.points.map((p) => p.v))
    if (this.yMax !== undefined && peak > this.yMax) this.yMax = this.niceCeiling(peak * 1.1)
  }

  // Rounds up to a number a person would choose for an axis: 150, not 144.
  niceCeiling(value) {
    const magnitude = Math.pow(10, Math.floor(Math.log10(value)))
    const normalised = value / magnitude
    const step = [ 1, 1.25, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10 ]
      .find((s) => normalised <= s) || 10
    return step * magnitude
  }

  showWarning(dropped) {
    if (!this.hasWarningTarget) return

    if (dropped > 0) {
      this.warningTarget.textContent =
        `${dropped.toLocaleString()} requests could not be started, because every ` +
        `simulated user was still waiting on a slow reply. That is a symptom of the ` +
        `server being slow, not of this machine running out of capacity.`
      this.warningTarget.hidden = false
    } else {
      this.warningTarget.hidden = true
    }
  }

  resize() {
    const canvas = this.canvasTarget
    const ratio = window.devicePixelRatio || 1
    const rect = canvas.getBoundingClientRect()

    // Back the canvas at device resolution, then work in CSS pixels — otherwise
    // every line is soft on a retina display.
    canvas.width = Math.round(rect.width * ratio)
    canvas.height = Math.round(rect.height * ratio)
    this.ctx = canvas.getContext("2d")
    this.ctx.setTransform(ratio, 0, 0, ratio, 0, 0)
    this.width = rect.width
    this.height = rect.height
  }

  tick(now) {
    // Clamp the step: a backgrounded tab can deliver a multi-second delta, and
    // advancing the playhead by that in one frame would jump exactly as badly
    // as the batching did.
    const delta = Math.min((now - this.lastFrameAt) / 1000, 0.1)
    this.lastFrameAt = now

    // The next frame is queued BEFORE this one is drawn, on purpose.
    //
    // Drawing used to be the last thing tick() did, so a throw inside draw()
    // ended the loop for good: the canvas kept whatever was on it and never
    // updated again. That failure is indistinguishable from a chart with no
    // data -- the server, the payload and the markup are all healthy, and the
    // frame just sits there. It is how a missing formatDelay presented for days,
    // as "the top chart doesn't populate". A frame that fails is now one lost
    // frame rather than the end of the animation.
    this.frame = requestAnimationFrame((next) => this.tick(next))

    try {
      this.advance(delta)
      this.draw()
    } catch (error) {
      // Reported once. At 60fps an unhandled draw error would otherwise bury the
      // console under thousands of copies of itself, and the one that matters is
      // the first.
      if (!this.reportedError) {
        this.reportedError = true
        console.error("coping-chart: a frame failed to draw", error)
      }
    }
  }

  // Moves the playhead forward in real time, correcting gently toward a fixed
  // distance behind the newest data.
  //
  // The correction is proportional rather than a snap: snapping to the target
  // each frame would reintroduce a jump every time a fetch landed.
  advance(delta) {
    if (this.points.length === 0) return

    const newest = this.points[this.points.length - 1].t

    // Never before the data begins.
    //
    // A series does not have to start at zero. The delivery-latency series
    // starts at the first message, which on a chat run is ten seconds in, after
    // everyone has finished joining -- and seeding the playhead relative to zero
    // put it *behind* the first point, so nothing was ever revealed and the
    // chart sat empty with a "—" readout while the payload held a full minute of
    // data.
    const oldest = this.points[0].t

    // Opening the page mid-run should show the history already recorded, not
    // replay it from the beginning.
    if (this.playhead === null) {
      this.playhead = Math.max(oldest, newest - this.buffer)
      return
    }

    // Once the run is over there is nothing more coming, so close the gap and
    // let the line finish where the data does.
    const goal = this.running ? newest - this.buffer : newest

    this.playhead += delta
    this.playhead += (goal - this.playhead) * Math.min(1, delta * 2)

    // Never draw ahead of the data, nor behind where it starts.
    if (this.playhead > newest) this.playhead = newest
    if (this.playhead < oldest) this.playhead = oldest
  }

  palette() {
    const styles = getComputedStyle(document.documentElement)
    const read = (name, fallback) => styles.getPropertyValue(name).trim() || fallback
    return {
      ink: read("--text", "#1f1a12"),
      muted: read("--text-muted", "#7a7264"),
      good: read("--good", "#2f6f3e"),
      bad: read("--danger", "#dc2626")
    }
  }

  draw() {
    const ctx = this.ctx
    if (!ctx) return

    const { ink, muted } = this.palette()
    const left = 52, right = 12, top = 12, bottom = 34
    const w = this.width, h = this.height
    const plotW = w - left - right
    const plotH = h - top - bottom

    ctx.clearRect(0, 0, w, h)

    if (this.points.length === 0) {
      ctx.fillStyle = muted
      ctx.font = "12px system-ui, sans-serif"
      ctx.textAlign = "center"
      // A chat run opens with a join window in which no message exists yet, so
      // this chart is legitimately empty for its first ten seconds. Saying so
      // beats an unexplained blank frame, which reads as broken.
      ctx.fillText(
        this.seriesValue === "latency"
          ? "nobody has spoken yet — waiting for the first message…"
          : "waiting for the first second of data…",
        w / 2, h / 2
      )
      return
    }

    // Fixed for the run. The whole test is on screen from the first frame, so
    // the line grows into a stationary frame rather than the frame chasing it.
    const xMin = 0
    const xMax = this.xMax || 50
    const yMax = this.yMax || Math.max(1, ...this.points.map((p) => p.v))

    const x = (t) => left + ((t - xMin) / (xMax - xMin || 1)) * plotW
    const y = (v) => top + plotH - (v / yMax) * plotH

    ctx.lineWidth = 1
    ctx.font = "10px system-ui, sans-serif"
    for (let i = 0; i <= 4; i++) {
      const value = (yMax * i) / 4
      const py = Math.round(y(value)) + 0.5

      ctx.strokeStyle = muted
      ctx.globalAlpha = 0.18
      ctx.beginPath()
      ctx.moveTo(left, py)
      ctx.lineTo(w - right, py)
      ctx.stroke()

      ctx.globalAlpha = 1
      ctx.fillStyle = muted
      ctx.textAlign = "right"
      const label = this.seriesValue === "latency"
        ? (yMax >= 2000 ? `${(value / 1000).toFixed(1)}s` : `${Math.round(value)}ms`)
        : Math.round(value)
      ctx.fillText(label, left - 8, py + 3)
    }

    ctx.textAlign = "center"
    for (let t = Math.ceil(xMin / 10) * 10; t <= xMax; t += 10) {
      ctx.fillText(`${Math.round(t)}s`, x(t), h - bottom + 16)
    }

    ctx.font = "600 10px system-ui, sans-serif"
    ctx.fillText("seconds since the test started", left + plotW / 2, h - 6)
    ctx.save()
    ctx.translate(12, top + plotH / 2)
    ctx.rotate(-Math.PI / 2)
    ctx.fillText(this.yAxisLabel(), 0, 0)
    ctx.restore()

    // The target is piecewise linear by construction — a ramp, then a flat hold.
    // Smoothing it would round a corner that is exactly square in reality.
    const upTo = this.playhead === null ? Infinity : this.playhead
    const revealed = this.points.filter((p) => p.t <= upTo)

    // Shade the gap before drawing the lines, so the lines stay legible on top.
    this.fillGap(revealed, x, y)

    this.stroke(this.target, x, y, xMin, muted, 1, [5, 5], false)
    this.stroke(revealed, x, y, xMin, ink, 1.5, [], true)

    if (this.hasReadoutTarget && revealed.length > 0) {
      // Read from the playhead too, so the number and the line agree.
      const current = revealed[revealed.length - 1].v
      this.readoutTarget.textContent = this.currentReading(current)
    }
  }

  // What this chart's y-axis measures, said in words.
  yAxisLabel() {
    switch (this.seriesValue) {
      case "latency": return "seconds until a message arrives"
      case "requests": return "requests per second"
      default: return "messages delivered per second"
    }
  }

  // The figure above the chart, matching where the line currently is.
  currentReading(value) {
    switch (this.seriesValue) {
      case "latency": return this.formatDelay(value)
      case "requests": return `${value.toFixed(0)} requests per second`
      default: return `${value.toFixed(0)} messages per second`
    }
  }

  // The delay under the chart, said the way a person would say it.
  //
  // The unit changes with the size because the two ranges mean different things:
  // under a second is a number of milliseconds nobody notices, and above it the
  // conversation has stopped feeling live, which is a matter of seconds. Matches
  // the y-axis, which switches units at the same point.
  formatDelay(ms) {
    return ms < 1000
      ? `${Math.round(ms)} ms to arrive`
      : `${(ms / 1000).toFixed(1)} seconds to arrive`
  }

  // Shades the area between delivered and needed: green where the server is
  // ahead, red where it is behind.
  //
  // Filled as contiguous same-sign runs rather than per-segment quads — a
  // thousand tiny fills a frame would not hold 30fps — and split exactly at the
  // crossings, so a band never bleeds a few pixels of the wrong colour past the
  // point where the lines actually meet.
  fillGap(points, x, y) {
    if (points.length < 2 || this.target.length < 2) return

    const paired = this.pairWithTarget(points)
    if (paired.length < 2) return

    const { good, bad } = this.palette()
    const ctx = this.ctx

    let run = [ paired[0] ]
    let ahead = this.isGood(paired[0])

    const flush = (band, isAhead) => {
      if (band.length < 2) return
      ctx.beginPath()
      ctx.moveTo(x(band[0].t), y(band[0].delivered))
      for (let i = 1; i < band.length; i++) ctx.lineTo(x(band[i].t), y(band[i].delivered))
      for (let i = band.length - 1; i >= 0; i--) ctx.lineTo(x(band[i].t), y(band[i].needed))
      ctx.closePath()
      ctx.fillStyle = isAhead ? good : bad
      ctx.globalAlpha = 0.16
      ctx.fill()
      ctx.globalAlpha = 1
    }

    for (let i = 1; i < paired.length; i++) {
      const previous = paired[i - 1]
      const current = paired[i]
      const nowAhead = this.isGood(current)

      if (nowAhead === ahead) {
        run.push(current)
        continue
      }

      // The lines cross somewhere inside this segment. Interpolate the exact
      // point and use it to end one band and begin the next.
      const before = previous.delivered - previous.needed
      const after = current.delivered - current.needed
      const fraction = before / (before - after)
      const crossing = {
        t: previous.t + (current.t - previous.t) * fraction,
        delivered: previous.delivered + (current.delivered - previous.delivered) * fraction
      }
      crossing.needed = crossing.delivered // by definition, at a crossing

      run.push(crossing)
      flush(run, ahead)

      run = [ crossing, current ]
      ahead = nowAhead
    }

    flush(run, ahead)
  }

  // Which side of the reference line is the good side.
  //
  // Inverted for latency: more deliveries is better, but more delay is worse.
  // Without this the shading would tell you the opposite of the truth on one of
  // the two charts.
  isGood(point) {
    return this.seriesValue === "latency"
      ? point.delivered <= point.needed
      : point.delivered >= point.needed
  }

  // Matches each delivered point to the target at the same instant.
  //
  // The two series share a 33ms grid but start at different offsets, so this
  // interpolates rather than pairing by index — pairing by index would shear
  // the bands by a fraction of a second.
  pairWithTarget(points) {
    const out = []
    let j = 0

    for (const point of points) {
      while (j < this.target.length - 2 && this.target[j + 1].t < point.t) j++

      const a = this.target[j]
      const b = this.target[j + 1] || a
      const span = b.t - a.t
      const needed = span > 0
        ? a.v + (b.v - a.v) * Math.min(1, Math.max(0, (point.t - a.t) / span))
        : a.v

      out.push({ t: point.t, delivered: point.v, needed })
    }

    return out
  }

  stroke(series, x, y, xMin, color, width, dash, smooth) {
    if (!series || series.length === 0) return

    const pts = []
    for (const point of series) {
      if (point.t < xMin) continue
      pts.push({ x: x(point.t), y: y(point.v) })
    }
    if (pts.length === 0) return

    const ctx = this.ctx
    ctx.beginPath()
    ctx.setLineDash(dash)
    ctx.strokeStyle = color
    ctx.lineWidth = width
    ctx.lineJoin = "round"
    ctx.lineCap = "round"

    ctx.moveTo(pts[0].x, pts[0].y)

    if (!smooth || pts.length < 3) {
      for (let i = 1; i < pts.length; i++) ctx.lineTo(pts[i].x, pts[i].y)
    } else {
      // Quadratic curves through the midpoint of each pair, using the data
      // points themselves as control points. Every point still influences the
      // line and none are dropped -- the curve simply arrives at each one along
      // a tangent instead of a corner, which is what removes the sawtooth.
      //
      // Note this rounds the very tips of spikes slightly: the curve passes
      // through midpoints rather than exactly through every vertex. The values
      // are untouched, only the ink between them.
      for (let i = 1; i < pts.length - 1; i++) {
        const midX = (pts[i].x + pts[i + 1].x) / 2
        const midY = (pts[i].y + pts[i + 1].y) / 2
        ctx.quadraticCurveTo(pts[i].x, pts[i].y, midX, midY)
      }

      const last = pts[pts.length - 1]
      const penultimate = pts[pts.length - 2]
      ctx.quadraticCurveTo(penultimate.x, penultimate.y, last.x, last.y)
    }

    ctx.stroke()
    ctx.setLineDash([])
  }
}
