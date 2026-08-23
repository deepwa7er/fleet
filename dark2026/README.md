# Dark 2026 — JetBrains theme

A hand port of the VS Code built-in **Dark 2026** theme
(`theme-defaults/themes/2026-dark.json`, which layers on `dark_modern.json`
with GitHub-Dark-derived token colors) to the JetBrains platform. Built for
RubyMine but depends only on `com.intellij.modules.platform`, so it works in
any JetBrains IDE.

Two pieces, shipped as one plugin jar:

- `theme/dark2026.theme.json` — the UI theme: tool windows, project tree,
  tabs, toolbars, popups, status bar. Panels `#191A1B`, editor `#121314`,
  hairline borders `#2A2B2C`, accent blue `#3994BC`.
- `theme/dark2026.editor.xml` — the editor color scheme: the GitHub-Dark
  token palette (coral keywords `#ff7b72`, pale-blue strings `#a5d6ff`, blue
  constants `#79c0ff`, purple methods `#d2a8ff`, orange class names
  `#ffa657`, gray comments `#8b949e`), plus Ruby-specific attributes,
  console/ANSI colors, and VCS file-status colors for the project tree.
  Colors VS Code defines with alpha are pre-blended against the background,
  since JetBrains schemes take solid values.

Selecting the theme (Settings → Appearance → Theme → Dark 2026) applies the
bundled editor scheme automatically.

## Build & install

```bash
make jar      # zips META-INF/ + theme/ into dark2026-theme.jar
make install  # copies the jar into the newest ~/Library/.../RubyMine*/plugins, then restart RubyMine
```

The jar is a plain zip; it is not committed — `make jar` reproduces it
byte-for-byte from the three source files. For a non-RubyMine IDE, install
the jar via Settings → Plugins → ⚙ → Install Plugin from Disk.
