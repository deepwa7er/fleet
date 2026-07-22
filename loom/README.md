# Carousel

Your windows, arranged on the inside of a donut. You're in the middle.

Every window keeps the same frame — the full screen minus a small gap, like
a tiling WM with a single window. The rest of your windows wait off-screen on
a ring around you. Hold **⌥ (Option)** and scroll (either axis): the current
window slides off and the next one slides in, looping through every window.
Release ⌥ and the front window is just a normal, full-size window.

Windows are resized exactly once, when they join the ring; spins are
position-only moves, which is what keeps the animation fluid.

## Build & run

    make app      # builds Carousel.app (ad-hoc signed so the Accessibility grant sticks)
    make run      # opens it — all windows immediately move onto the ring
    make install  # copies it to ~/Applications

On first launch macOS asks for Accessibility access (needed to observe
scroll events and move windows). Grant it and Carousel starts by itself a
second later.

The ◎ menu-bar item offers **Restore Window Frames** (put everything back
where it was) and **Quit** (also restores before exiting).

## Notes

- All on-screen windows are gathered onto the primary display's ring;
  full-screen and minimized windows are left alone.
- New windows join at the back of the ring; closed windows drop out (checked
  every 2 s).
- While ⌥ is held, scroll events are consumed system-wide — apps that bind
  ⌥+scroll (e.g. zoom in Preview) won't see it.
- Trackpad momentum (coasting after a flick) is ignored so a hard flick
  doesn't spin the ring away.
- Tuning lives in `Sources/Carousel/CarouselLayout.swift` (the `gap` margin
  around the stage), `Carousel.pointsPerSlot` (scroll travel per window),
  `Carousel.snapEase` (snap glide speed), and
  `ScrollInterceptor.wheelNotchesPerSlot` (mouse-wheel sensitivity).
