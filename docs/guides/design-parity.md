# Design Parity Screenshots

Use this when comparing a rendered `kithara-ui` component against the same
component in the design-system handoff (`kithara-design-system/`, an untracked
working copy).

Both sides render to PNG at device-scale 2, byte-reproducibly. The comparison is
a **contact sheet plus two metrics**, never a cross-side pixel diff: iced/wgpu
and Chromium rasterise differently, so equality is not the goal — locating
displacement is.

## Why the two sides are comparable

- Same font files. The Rust side embeds `crates/kithara-ui/assets/fonts/*.ttf`;
  the harness below loads those same files through `@font-face`. Google Fonts
  serves a *variable* Space Grotesk, which does not match — never use the CDN
  stylesheet for parity work.
- Same device scale. The gallery captures at DPR 2 on macOS; Chrome is forced to
  `--force-device-scale-factor=2`.
- Flat design. Solid fills, 1px borders, zero radius. Colour histograms and
  difference overlays stay meaningful; only text and canvas art are
  antialiasing-sensitive.

## Prerequisites

- `kithara-design-system/` present in the repo root (untracked working copy).
- Google Chrome installed.
- `ffmpeg` on PATH (only image tool needed; `sips` for format conversion).

## 1 · Rust side

```bash
cargo build -p kithara-ui --example gallery --features render
```

```bash
KITHARA_SHOT_DIR=<dir> ./target/debug/examples/gallery
```

Walks every tab and writes `tab-<name>.bmp`, 2200x1440 (1100x720 logical at DPR
2), then exits after the last tab. One full pass is about a minute.

Output is byte-identical across runs, including the mixer tab with its VU
meters — verified with `shasum -a 256`. No tolerance threshold is needed for
Rust-vs-Rust regression comparisons.

Convert for viewing:

```bash
sips -s format png tab-modules.bmp --out tab-modules.png
```

### The studio window

`kithara-app` has the same env trigger; it captures the studio window once and
exits.

```bash
KITHARA_SHOT_DIR=<dir> ./target/debug/kithara --mode gui
```

Writes `studio.bmp` at 2560x1520 — the window is a fixed 1280x760
(`STUDIO_WIDTH`/`STUDIO_HEIGHT` in `frontend.rs`), so the design case below is
sized to match and the two frames overlay directly with no crop. Change those
constants and the `studio` case's `w`/`h` must follow, or the overlay needs a
crop again. The capture fires on tick 40; the binary is named `kithara`, not
`kithara-app`.

## 2 · Design-system side

Write `harness.html` into `kithara-design-system/` (relative paths only, so a
fresh handoff just needs the file re-created). It mounts **one** component from
a named case, with time and randomness frozen.

```html
<!DOCTYPE html>
<html lang="ru">
<head>
<meta charset="UTF-8" />
<title>Kithara design-system harness</title>

<style>
  @font-face { font-family: "Space Grotesk"; font-weight: 400; src: url("../crates/kithara-ui/assets/fonts/SpaceGrotesk-Regular.ttf"); }
  @font-face { font-family: "Space Grotesk"; font-weight: 500; src: url("../crates/kithara-ui/assets/fonts/SpaceGrotesk-Medium.ttf"); }
  @font-face { font-family: "Space Grotesk"; font-weight: 600; src: url("../crates/kithara-ui/assets/fonts/SpaceGrotesk-SemiBold.ttf"); }
  @font-face { font-family: "Space Grotesk"; font-weight: 700; src: url("../crates/kithara-ui/assets/fonts/SpaceGrotesk-Bold.ttf"); }
  @font-face { font-family: "Inter"; font-weight: 400; src: url("../crates/kithara-ui/assets/fonts/Inter-Regular.ttf"); }
  @font-face { font-family: "Inter"; font-weight: 600; src: url("../crates/kithara-ui/assets/fonts/Inter-SemiBold.ttf"); }
  @font-face { font-family: "JetBrains Mono"; font-weight: 400; src: url("../crates/kithara-ui/assets/fonts/JetBrainsMono-Regular.ttf"); }
  @font-face { font-family: "JetBrains Mono"; font-weight: 500; src: url("../crates/kithara-ui/assets/fonts/JetBrainsMono-Medium.ttf"); }
  @font-face { font-family: "JetBrains Mono"; font-weight: 600; src: url("../crates/kithara-ui/assets/fonts/JetBrainsMono-SemiBold.ttf"); }
  html, body { margin: 0; padding: 0; background: #0b0b16; }
  #stage { position: absolute; left: 0; top: 0; }
</style>

<script>
  const HARNESS_FRAMES = 1;
  const HARNESS_T = 1000;
  let harnessFrames = 0;
  const rawRaf = window.requestAnimationFrame.bind(window);
  window.requestAnimationFrame = (cb) => {
    if (harnessFrames++ >= HARNESS_FRAMES) return 0;
    return rawRaf(() => cb(HARNESS_T));
  };
  window.cancelAnimationFrame = () => {};
  let seed = 0x2f6e2b1;
  Math.random = () => { seed = (seed * 1664525 + 1013904223) >>> 0; return seed / 4294967296; };
  Date.now = () => 1700000000000;
</script>

<link href="https://unpkg.com/lucide-static@0.462.0/font/lucide.css" rel="stylesheet" />
<link rel="stylesheet" href="src/styles/tokens.css" />
</head>
<body>
<div id="stage"></div>

<script src="https://unpkg.com/react@18.3.1/umd/react.development.js"></script>
<script src="https://unpkg.com/react-dom@18.3.1/umd/react-dom.development.js"></script>
<script src="https://unpkg.com/@babel/standalone@7.29.0/babel.min.js"></script>

<!-- system → modules → engine → registry; same order as index.html,
     minus app.jsx and the concepts/NN-*.jsx layouts -->
<script type="text/babel" src="src/system/tokens.jsx"></script>
<script type="text/babel" src="src/system/atoms.jsx"></script>
<script type="text/babel" src="src/system/controls.jsx"></script>
<script type="text/babel" src="src/system/parts.jsx"></script>
<script type="text/babel" src="src/system/blocks.jsx"></script>
<script type="text/babel" src="src/modules/strip.jsx"></script>
<script type="text/babel" src="src/modules/deck.jsx"></script>
<script type="text/babel" src="src/modules/mixer.jsx"></script>
<script type="text/babel" src="src/modules/library.jsx"></script>
<script type="text/babel" src="src/modules/global-bar.jsx"></script>
<script type="text/babel" src="src/engine/core.jsx"></script>
<script type="text/babel" src="src/engine/canvas.jsx"></script>
<script type="text/babel" src="src/engine/visualizer.jsx"></script>
<script type="text/babel" src="src/engine/input.jsx"></script>
<script type="text/babel" src="src/engine/data-library.jsx"></script>
<script type="text/babel" src="src/engine/data-tracklist.jsx"></script>
<script type="text/babel" src="src/engine/bindings-transport.jsx"></script>
<script type="text/babel" src="src/engine/bindings-layout.jsx"></script>
<script type="text/babel" src="src/engine/bindings-instrument.jsx"></script>
<script type="text/babel" src="src/engine/bindings-mixer.jsx"></script>
<script type="text/babel" src="src/engine/bindings-session.jsx"></script>
<script type="text/babel" src="src/concepts/registry.jsx"></script>

<script type="text/babel">
const CASES = {
  deck: {
    w: 895, h: 613,
    state: { playA: false, hideA: false, syncA: true, revA: false },
    view: (v, c) => <Deck size="md" w={c.w} h={c.h} d={{
      cref: v.refWaveA, enter: v.waveEnterA, leave: v.waveLeaveA,
      op: v.infoAOp, y: v.infoAY,
      title: 'MoonShine_Секрет', artist: 'teo_van_bo',
      bpm: '70.00', key: '4m', remain: '-04:17', L: 'A',
      playBg: v.playABg, playFg: v.playAFg, onPlay: v.togglePlayA,
      syncBg: v.syncABg, syncFg: v.syncAFg, onSync: v.toggleSyncA,
      revBg: v.revABg, revFg: v.revAFg, onRev: v.toggleRevA,
    }} />,
  },
};

/* The `studio` case composes the same modules concept 05 does — global bar,
   two overview rows, deck | mixer | deck, library, foot bar — inside a
   CornerFrame sized to the app window (1280x760). Copy the composition from
   src/concepts/05-two-decks.jsx and pass `w: c.w`. */

const name = new URLSearchParams(location.search).get('case') || 'deck';
const c = CASES[name];
window.KConcepts.harness = ({ v }) => c.view(v, c);

const stage = document.getElementById('stage');
stage.style.width = c.w + 'px';
stage.style.height = c.h + 'px';

class Harness extends KitharaEngine {
  constructor(props) {
    super(props);
    this.state = { ...this.state, ...(c.state || {}) };
    if (c.pos) Object.assign(this._pos, c.pos);
    if (c.loops) Object.assign(this.loops, c.loops);
  }
}
Harness.defaultProps = { ...KitharaEngine.defaultProps, concept: 'harness' };

ReactDOM.createRoot(stage).render(<Harness />);

document.fonts.ready.then(() => {
  setTimeout(() => { window.__harnessReady = true; document.title = 'ready:' + name; }, 300);
});
</script>
</body>
</html>
```

Capture (`--window-size` must equal the case's `w`/`h`):

```bash
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu --hide-scrollbars --allow-file-access-from-files --force-device-scale-factor=2 --window-size=895,613 --virtual-time-budget=8000 --screenshot=ds-deck.png "file://$PWD/kithara-design-system/harness.html?case=deck"
```

Byte-identical across runs — verified by hashing two consecutive captures.

### Harness rules that matter

- **Case state goes through the constructor**, not `setState` after mount: a
  post-mount `setState` races the first paint and silently does nothing.
- **`KitharaEngine` draws canvases in `componentDidMount` via `drawAll()`**, so a
  single frozen rAF frame is enough. `HARNESS_FRAMES = 1` still lets VU meters
  paint (they only draw inside the tick).
- **Waveforms are seeded** (`rng(seed)` in `engine/canvas.jsx`), so shape is
  stable; the playhead is not — it advances in the rAF tick, so any case that
  compares a deck must set `playA: false` and pin `_pos`.
- `Math.random` drives VU levels; the seeded replacement above must be installed
  before `core.jsx` loads.
- When a new handoff lands, re-check the script list against `index.html`:
  ```bash
  grep -E '<script type="text/babel"' kithara-design-system/index.html | grep -vE 'concepts/[0-9]|app\.jsx'
  ```

### Still on the CDN

React, Babel and the lucide icon font are still fetched from unpkg. Fine for a
manual pass; vendor them before any of this becomes an automated check, since
tests must not depend on the network.

## 3 · Align the frame

The gallery renders the component inside a tab, so crop it to the same box as
the harness output. For the deck at a 1100x720 gallery window:

```bash
ffmpeg -y -i tab-modules.bmp -vf "crop=1790:1226:399:164" rs-deck.png
```

Finding the box for a new tab: read the tab's `klayout.ron` in
`crates/kithara-ui/examples/gallery/assets/` for the fixed chrome around the
component (gallery sidebar, tab strip, section header), double those logical
values for DPR 2, and verify by viewing the crop — a correct box has the
component's own border on the outermost pixel row. Then size the harness case to
the cropped box (895x613 logical = 1790x1226 physical for the deck); matching
sizes are what make every later step a straight overlay.

## 4 · Compare

**Contact sheet** — the primary artifact, read by eye:

```bash
ffmpeg -y -i ds-deck.png -i rs-deck.png -filter_complex "[0:v]pad=1790:1266:0:40:0x0b0b16[a];[1:v]pad=1790:1266:0:40:0x0b0b16[b];[a][b]hstack=inputs=2" side-by-side.png
```

**Difference overlay** — locates displacement; white means agreement, black
marks where the two renders disagree:

```bash
ffmpeg -y -i ds-deck.png -i rs-deck.png -filter_complex "[0:v][1:v]blend=all_mode=difference,format=gray,eq=contrast=5:brightness=0.02,negate" diff-deck.png
```

Doubled glyph outlines are the useful signal: a doubled label means its box or
baseline moved. Crop bands out of `diff-deck.png` to read them
(`crop=1790:230:0:0` for the info overlay, `crop=1790:120:0:1106` for the
transport bar).

**Structural report** — `tools/design-parity/parity.py`, the one that finds
discrepancies instead of confirming them. It reads where colour *changes*, so
Chromium and wgpu rasterising differently does not disturb it, and it answers
the three questions eyeballing keeps missing:

```bash
python3 tools/design-parity/parity.py bands ds-studio.png ours.png
python3 tools/design-parity/parity.py strip ds-studio.png ours.png 0 92 200 70
python3 tools/design-parity/parity.py ink   ds-studio.png ours.png 2 88 56 80
```

- `bands` — every horizontal boundary with its colour. A stack of two lines
  where the canon has one is a doubled divider; a strip 2px shorter is a height
  that lost pixels to a border.
- `strip` — runs of columns across a band: cell edges, widths and fills. A cell
  present on one side only is an element we do not have, not a metric mismatch —
  read it as a gap in coverage and move on.
- `ink` — air between a cell's edges and its content, per side. This is what
  catches a glyph glued to its border: canon `left=21 right=21` against ours
  `left=0 right=39` says the text is not centred, and no amount of staring at
  the screenshot says it as plainly.

Run `bands` first — it gives the Y coordinates every other call needs.

```bash
ffmpeg -loglevel error -i rs-deck.png -f rawvideo -pix_fmt rgb24 - | xxd -p -c 3 | sort | uniq -c | sort -rn | head -12
```

Compare the two lists as sets of `#rrggbb` against `tokens.css` / the skin
palette. Exact token values on both sides mean flat fills landed correctly;
near-but-not-equal values mean something is being blended.

## What the first pass found

Running the deck case end to end surfaced, without any tuning:

- Info-overlay text is displaced between the two renders — title, artist, `70.00`,
  `-04:17` and the deck letter all double in the difference overlay, while the
  BPM/KEY box borders align. Boxes agree, contents do not.
- The Rust transport bar carries controls the design's deck does not place there
  (zoom pair, HLS cell); in the design these arrive through the deck's `tail`
  slot.
- The waveform bed differs by one step: `#121223` (design) against `#111123`
  (ours). Both are derived values — neither appears in `tokens.css` or the skin
  palette — so the source is not yet identified.

The same run also produced a false finding, worth keeping as the cautionary
example. Full-frame histograms showed our waveform colours shifted off the
tokens (`#f2d129 → #e6c42f`, `#eb298c → #e0417c`, `#2ec7eb → #4dbcc6`) — but the
loop region was active on the Rust side only, and its `#3a312f` overlay covered
the bars being measured. Re-reading the histogram over a band free of the
overlay (`crop=880:900:10:220`) shows both sides agreeing within one unit
(`#2285a1`/`#2285a0`, `#9d1e63`/`#9d1f63`, `#a18c22` on both). Pin case state, or
measure only where the frames are known to agree.

## Scope

- Canvas-drawn art (waveform, VU, knobs) is compared by eye only; its model
  belongs in unit tests.
- Cross-side pixel thresholds are not a goal and should not be added.
- Rust-vs-Rust baseline comparison can be exact, since captures are
  byte-reproducible.
