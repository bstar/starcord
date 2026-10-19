# STAR/CORD visual treatments

Open `index.html` directly in a browser, or serve this directory:

```sh
python -m http.server 4187 --bind 127.0.0.1 --directory design
```

Then visit <http://127.0.0.1:4187>. No build, account, network assets or Discord
connection is needed. The gallery includes ten treatments, four themes (Kanagawa by default),
four conversation states, a reader view and a browser-local shortlist. A link
such as `index.html#treatment-7` opens a specific treatment. Drafts last for
the current page session; only shortlisted treatment IDs use local storage.

These are **HTML visual proposals**. The selected Focus Signal direction is
now implemented through KIT's native workspace surfaces in
`src/ui/app/native.rs`, with the frontend/session launcher in `src/graphical.rs`.
The CSS remains a visual study; it is not used by the Rust renderer. The
application shares its existing layout, widgets, core, navigation and drafts
between presentations. KIT's graphics-only renderer is the default `native`
feature; `--no-default-features` keeps the cell-only build.

## Treatment map

| # | Treatment | Shared KIT construction | What changes |
|---|---|---|---|
| 01 | Classic rack | `Style::panel`, inset `Style::frame` | Original raised shell and dark wells |
| 02 | Rigid instrument | `Style::frame_at_density` with `rigid = true` | Square frames and stronger seams |
| 03 | Quiet workspace | `Style::workspace_panel`, `Style::tool` | Neutral flat panels and fine separators |
| 04 | Deep wells | `Style::panel`, inset frames | Deeper recess and wider content lip |
| 05 | Conversation ledger | Workspace panels, shared line roles | Joined panes, ruled rows, aligned times |
| 06 | Soft-spoken | Workspace surfaces and sections | Minimal edges and plain author initials |
| 07 | Focus signal | Workspace panels, `Style::focus` vocabulary | Strong focus outline and selected title |
| 08 | Reading room | Classic panels, shared spacing metrics | More generous gutters, gaps and leading |
| 09 | Clear signal | Shared foreground, border and accent roles | Strong flat boundaries and brighter metadata |
| 10 | Family standard | Classic shell, workspace interiors | One pane recipe combining the family conventions |

**Selected direction: 07 / Focus signal, with Kanagawa.** The gallery opens
with this treatment and palette by default. All five modules share KIT's quiet
workspace construction; the active module gets a clear accent outline and
selected title fill. Row selection remains independent of keyboard focus.
Neutral content surfaces stay consistent across servers, channels, conversation,
composer and members.

The other nine treatments remain available for comparison. Treatment 01 is the
closest Classic Rack match; 03 is the closest quiet REEL workspace match.

## Source of truth

Reviewed against sibling checkouts at KIT candidate
`183f298a143efbc6b90e30ce22013dcc8e5d2e69`, matching CORD's pinned dependency:

- `starkit/docs/native-style.md`: shared visual language and frontend contracts.
- `starkit/src/native_surface/style.rs`: `Palette`, `Style`, Classic colors,
  frame construction, physical type sizes and metrics.
- `starkit/src/native_surface/style/workspace.rs`: derived workspace colors,
  32 px headers, quiet controls, shared UI fonts and modal construction.
- `starfold/src/ui/rack.rs`: FOLD's adapter over shared KIT chrome.
- `starfold/docs/graphical/treatments/`: Classic Commander visual study.
- `starreel/src/ui.rs` and `src/ui/shell.rs`: workspace panels used throughout
  the editor and desktop shell.
- `starcord/src/ui/layout.rs` and `src/ui/panels/mod.rs`: current accordion,
  module order, focus and shared terminal frame adapter.

`kit-palettes.js` is generated using the **pinned Rust dependency**, including
KIT's Oklab-derived workspace roles, theme contrast floors and selection
derivation. Kanagawa uses AMP and FOLD's graphical adapter: the resolved
header background supplies slate frames and a darkened theme background supplies
inset wells. The gallery shell follows the selected palette too. It is not an independent set of CORD theme definitions. Regenerate
from the repository root with:

```sh
nix develop -c cargo run --quiet --example export-design-palette \
  | sed -n '/^\/\/ Generated/,$p' > design/kit-palettes.js
```

On machines without experimental features enabled, add
`--extra-experimental-features 'nix-command flakes'` immediately after `nix`.
The filter excludes devshell banners. The preview bundles KIT's Liberation
Sans and Mono font files with their OFL license in `assets/FONT-LICENSE.txt`.
Native Classic panels retain their configured font contract; the browser
uses KIT's bundled UI faces consistently to isolate pane treatment choices.

## Implementation boundary after choosing

1. Keep one CORD pane adapter over `starkit::native_surface::style::Style`.
   All five modules consume it. Palette, artwork, fonts, controls, focus and
   spacing belong to KIT; CORD owns labels, messages and application actions.
   A generally useful new style capability belongs in KIT and requires checking
   its other consumers, rather than adding per-application RGB tables.
2. Preserve `LayoutState`'s full-width accordion and the reader behavior.
   KIT `panes` supplies solved geometry for drawing, focus and hit testing.
   Keep user collapse distinct from a small-viewport fallback. Browser reader
   mode illustrates reduced chrome, not a new application setting.
3. Keep the graphical path optional and retain terminal rendering, media,
   SSH and presentation contracts. Negotiate `native_skins`; use the shared
   primitive fallback when unavailable. Scale geometry, text and hit regions
   together, using `Style::mount` for graphical placement.
4. Continue querying `Handle` and `State` from the UI. Nothing under
   `src/discord/` acquires presentation imports or automated actions.
5. Preserve chat theme roles and contrast derivation for actual message
   rendering. These studies show shared chrome roles; semantic message colors
   must continue to use CORD's existing `Theme.chat` roles during implementation.
6. Keep a future KIT dependency update in its own commit with manifest and
   git-sourced lockfile together. No local path override is required here.

The preview uses synthetic conversation data. Its sample buttons show local
feedback, reactions toggle locally and Enter retains a sample draft. It does
not search, upload or send. Only the pane header focus/fold behavior is modeled;
the HTML gallery is not a replacement implementation of the full TUI keymap.

## Native verification

The implementation uses the pinned KIT candidate without a sibling path patch.
Native and cell presentations share `App`, the pane tree, content widgets and
input actions. KIT's public client/session APIs handle the frontend, F9,
reconnection and image recovery. No browser runs in the application.

Full pictures use up to eight reused KIT thumbnail workers and an 8 MB encoded
scene budget. A partially scrolled picture or a picture beyond that budget
retains the existing clipped cell paint. User-defined chat colors remain
authoritative; derived chat roles are resolved against the native well.

Checks from the repository root:

```sh
nix develop -c cargo test --all
nix develop -c cargo test --no-default-features
nix develop -c cargo clippy --all-targets --all-features -- -D warnings -A dead_code
nix develop -c cargo build --release
```

The Linux terminal check needs Kitty/kitten 0.49 or later for screenshot capture:

```sh
nix develop -c python3 scripts/test-native.py \
  --binary target/release/starcord --kitty "$KITTY" --kitten "$KITTEN" \
  --output /tmp/starcord-native-proof
```

It uses private data, configuration and runtime directories, a synthetic QR
login and a static copy of the replay. It checks navigation, help, a draft
across F9, Ctrl+L, recovery after terminal image loss and confirmation on quit.
Screenshots and logs remain in the selected output directory. No account or
real session is involved. Physical macOS and other terminal implementations
are separate acceptance work.
