# Tiler

One window at a time, filling the screen. Every window on the managed display
gets the same frame — the full screen minus a small gap, like a tiling WM with
a single window — and the rest wait behind it. **⌘1–9** brings a window to the
front; **⌥⌘1–9** gives the front window a number.

Windows are resized exactly once, when they join. Switching is a raise and a
focus change, so nothing moves and no app re-layouts.

## Build & run

    make app      # builds Tiler.app (signed so the Accessibility grant sticks)
    make run      # opens it — all windows on the display immediately tile
    make install  # copies it to ~/Applications

On first launch macOS asks for Accessibility access (needed to observe key
events and move windows). Grant it and Tiler starts by itself a second later.

## Keys

| | |
|---|---|
| `⌘1`–`⌘9` | bring the numbered window to the front |
| `⌥⌘1`–`⌥⌘9` | give the front window that number |
| `⌥Space` | toggle the switcher panel (click a row to switch) |
| 🎤 (top-row F5) | toggle the command panel — the menu, as a window |

Clicking a row focuses that window and the panel stays put, so windows can be
picked one after another. The panel sits above every ordinary window, so the
window you pick is raised to just beneath it rather than over it.

**Right-click a row** to send that window to another display. The display
section turns into a picker naming the window — the display it is already on is
left out, since sending it there is a no-op dressed up as a choice — and it
lands on the destination's stage, the same frame it would get if Tiler managed
it there. Tiler manages one display at a time, so the window then leaves the
stack and its row drops out.

Hovering a row reveals a **✕** at its trailing edge, which closes that window —
the same thing ⌘W does, by pressing the window's own close button. It is not a
process kill: the app decides what closing means, so unsaved-work prompts still
appear, an app refusing to close is allowed, and an app left with no windows
carries on running as usual. Windows without a close button — dialogs, some
panels — report the failure rather than pretending.

The panel keeps the top of the stack until it is explicitly toggled away —
clicking into another app does not dismiss it, so windows can be focused and
worked with underneath it. The ways out are the microphone key, `Esc`, and the
◎ menu item.

In the command panel the window column is always in ⌘-digit order, and **the
order is the numbering**: drag a row and every digit is rewritten from the new
top-to-bottom order, so the first row is ⌘1, the second ⌘2, and so on. Past the
ninth row a window holds no digit — there are only nine keys. Dragging is a
wholesale renumbering rather than the swap ⌥⌘1–9 performs, because the drag is a
statement about the whole column.

The microphone key is claimed outright. It arrives as an ordinary key-down
carrying keycode 176 — not as an `NX_SYSDEFINED` media event like volume or
brightness — so the existing tap already sees it, and swallowing it there is
what stops Dictation: the system's handler sits downstream of the session tap.
Nothing is remapped and nothing has to be switched off in System Settings. Turn
on "Use F1, F2, etc. as standard function keys" and the same physical key sends
F5 (96) instead, which this binding does not cover.

⌘-digits only bind while the pointer is on the managed display — everywhere
else the keystroke goes to the app, so `⌘1` still switches browser tabs on
your other screen. ⌥Space is deliberately not gated: it's how you reach the
stack when the pointer isn't on it.

The command panel (🎤) is a borderless window opened at the pointer: the managed
windows with their digits, a display picker, restore frames, start at login and
quit. Being borderless it has no title bar, and therefore no close/minimise/zoom
buttons at all.

The ◎ menu-bar item lists the windows with their digits, picks the managed
display, offers **Start at Login**, and **Quit** (which puts every window back
where it was found).

## Starting at login

`make install`, launch the copy in `~/Applications`, and tick **Start at
Login** in the ◎ menu. It registers the bundle with `SMAppService`, so it
shows up in System Settings → General → Login Items and can be switched off
there; the menu reads its state back from the system rather than caching it.
If it has been switched off in Settings the menu item says so, and clicking it
opens that pane — an app can't re-enable itself once the user says no.

Enable it on the installed copy, not on the `Tiler.app` in this repo: the
registration records the bundle's path, and `make app` deletes and rebuilds
that one. Deleting a registered bundle for good leaves a dead login item
(status `.notFound`) that has to be cleared in System Settings.

Accessibility access survives, since the grant is keyed to the signing
identity. If it hasn't been granted yet the app still starts and waits,
polling once a second, so a login launch needs no relaunch after you grant it.

## Notes

- Only one display is managed at a time — pick it under ◎ → Display. Windows
  on other displays keep their own frames and are never touched.
- Full-screen and minimized windows are left alone.
- New windows join at the back and take the lowest free digit; closed windows
  drop out. Membership is reconciled twice a second, and paused while the
  screen is locked (the WindowServer stops reporting the session's windows
  then, and a pass taken during a lock would evict everything).
- ⌘-digit assignments persist by app bundle ID, so "RustRover is ⌘1" survives
  a restart. A digit reserved for an app that isn't running is held for it.
- Tiling happens at enrollment and when the display geometry changes. A window
  you drag or resize yourself is left where you put it rather than snapped
  back — **Retile all** in the command panel squares the whole stack up again,
  front window first. The stage is the visible area inset by 12pt on all four
  sides, so it is centred by construction — 2pt tighter than Rectangle's
  maximize, which is where the habit of pressing ⌃⌥Enter came from. Apps
  that refuse to resize past their own limits keep their size and are simply
  moved, exactly as at enrollment.
- The stage gap is 12pt on every side, in `Sources/Tiler/StageLayout.swift`.
