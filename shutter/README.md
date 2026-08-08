# Shutter

A screenshot tool for macOS: capture a region, a window, or a whole display,
draw red rectangles on the result, then copy it and save it.

Menu-bar only — no Dock tile, no menu bar of its own.

## Using it

Shutter answers the system's own screenshot shortcuts:

| | |
| --- | --- |
| **⌘⇧4** | selection overlay |
| **⌘⇧5** | selection overlay (the system's mode toolbar; the overlay's mode switching is the equivalent) |
| **⌘⇧3** | grab the display under the cursor, straight to the editor |

⌃⌘⇧3 and ⌃⌘⇧4 are left alone, so the system's copy-to-clipboard variants still
work as they always did.

Press **⌘⇧4**. The screen freezes and dims.

| In the overlay | |
| --- | --- |
| drag | select a region |
| `Space` | switch to the window picker — click a window to capture it |
| `Tab` | switch to display mode; press again to cycle displays |
| `Return` | capture the highlighted display |
| `Esc` | cancel |

A magnifier follows the cursor in region mode, showing the pixels under it and
the coordinate you are on. The readout beside a selection is its size in pixels.

The editor opens on every capture.

| In the editor | |
| --- | --- |
| drag | draw a red rectangle |
| click a rectangle's edge | select it; drag to move it |
| `⌫` | delete the selected rectangle |
| `⌘Z` / `⇧⌘Z` | undo / redo |
| `⌘C` | copy to the clipboard |
| `⌘S` | save a PNG |
| `Return` | save, copy, and close |
| `Esc` | discard immediately, no confirmation |

Saves go to the Desktop by default, named `Shutter <date> at <time>.png`. Change
the folder from the menu bar item.

Copy is file-backed, like copying the file in Finder: pasting into a terminal or
text field gives the PNG's path, while image-aware apps and terminals that
understand image data get the image itself.

## Building

```
make app        # build and sign Shutter.app in place
make run        # ... and launch it
make install    # copy it to ~/Applications
```

The bundle is signed with a stable Apple Development identity rather than
ad-hoc. TCC keys the Screen Recording grant to the code signature, and an
ad-hoc signature changes on every build — you would be re-granting the
permission after every `make`.

## Taking over ⌘⇧3/4/5

⌘⇧3/4/5 are *symbolic hot keys*: the window server hands them to the system
screenshot service before any application hot key is dispatched.
`RegisterEventHotKey` for those combinations succeeds, returns `noErr`, and then
never fires. The only way for Shutter to answer them is for the system to stop
taking them, which means `CGSSetSymbolicHotKeyEnabled` — **private SkyLight
API**, resolved through `dlsym` so that a future macOS dropping it leaves
Shutter running with a diagnostic rather than failing to launch.

That switch outlives the process, so the state of each key is recorded in
preferences *before* it is touched:

- Quitting restores each key to what it was — a shortcut you had already turned
  off in System Settings stays off.
- A crash cannot run that restore, so a record still present at the next launch
  is restored from before a fresh takeover. An unclean exit repairs itself on
  the next run.
- **Use Shutter for ⌘⇧3/4/5** in the menu turns the takeover off and on without
  quitting.

If Shutter is force-killed and never launched again, ⌘⇧3/4/5 stay off until you
re-tick them under System Settings → Keyboard → Keyboard Shortcuts →
Screenshots. That is the one failure mode this design has.

The takeover only happens once Screen Recording is granted — displacing the
system tool before Shutter can capture anything would leave you with no working
screenshot tool at all.

## Permissions

Screen Recording, and nothing else. macOS only applies that grant to a newly
launched process, so the first run asks for it, notices when it lands, and
offers to relaunch.

Launch it as `Shutter.app`, not as the bare binary from a terminal: run from a
terminal it inherits the *terminal's* TCC identity, so it will appear to work
without the app itself ever being granted anything.

## How it captures

The whole screen is captured **first**, the moment the hot key fires, and the
overlay is a picture of that frozen screen. Selecting afterwards means the
pixels you dragged a box around are exactly the pixels you get: no second
capture that could race a moving screen, and no dance of hiding the overlay and
hoping the window server has caught up before the shutter fires. It is also
what gives the magnifier real pixels to zoom into.

Region and display captures are crops of that frozen image. Window capture is
the exception — it takes a fresh, desktop-independent capture of just that
window, so a partly covered window comes out whole instead of with whatever was
sitting on top of it baked in. The cost is that a window playing video may hand
back a frame slightly later than the one that was highlighted.

## Known limits

- A region drag is clamped to the display it started on. One capture cannot
  span two monitors.
- Rectangles can be drawn, moved, and deleted, but not resized — undo and
  redraw instead.
- Red rectangles are the only markup. There is no text, arrow, blur, or crop
  tool.
- The shortcuts are fixed at ⌘⇧3/4/5 and cannot be rebound.
- ⌘⇧3 captures the display under the cursor. The system tool writes every
  display to its own file; Shutter opens one editor instead.
