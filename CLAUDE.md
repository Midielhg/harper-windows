# Harper for Windows — working notes

A fork of [Automattic's Harper](https://github.com/automattic/harper) that makes
Harper Desktop work properly on Windows. Upstream's desktop app targets macOS;
on Windows the overlay existed but the OS integration behind it did not.

Fork: <https://github.com/Midielhg/harper-windows> (remote `fork`, branch `main`).
Upstream is remote `origin` and is never pushed to.

Read `AGENTS.md` first — it is upstream's own guide to the repository and still
applies. This file covers only what is specific to the Windows work.

## Where the Windows work lives

Everything is under `harper-desktop/src-tauri/src/`:

| Path | What it does |
| --- | --- |
| `windows_broker/mod.rs` | The `OsBroker` for Windows: reads the focused app, paces those reads, tracks the selection and the window's position |
| `windows_broker/automation_service.rs` | The worker thread that talks to UI Automation, and the code that applies fixes |
| `windows_broker/clipboard.rs` | Puts a replacement on the clipboard for a paste, then puts back what was there |
| `highlighter/render_state/` | What is drawn: underlines, the hover card, the review panel |
| `highlighter/window_manager.rs` | The event loop, hit-testing, and where a read turns into what is on screen |
| `highlighter/fix_all_hotkey.rs` | The low-level keyboard hook behind the Tab shortcut |

## The one thing to understand first

**Every read of the focused app is served by that app's own UI thread.** UI
Automation is a cross-process call, and the app answers it on the same thread it
uses to take keystrokes and draw. So:

- Reading harder makes the app *feel slower to type in*. That is why reads are
  paced by `READ_INTERVAL` (160ms) and backed off further by
  `READ_DUTY_DIVISOR`, which keeps Harper to roughly a quarter of that thread
  however slow the app is.
- **A busy app is slowest to answer at exactly the moment you most need it** —
  while it is being dragged, or while it is applying an edit. Anything that must
  stay correct during those moments cannot be built on a read.
- A read can simply never come back. Every wait on the worker is bounded
  (`WORKER_TIMEOUT`, 250ms) with `try_send`/`try_recv`/`recv_timeout`. An
  unbounded wait here froze the whole overlay and needed an app restart.

When something needs to be cheap and reliable, prefer a plain Win32 call that
needs no cooperation from the app — `GetWindowRect` and `GetForegroundWindow`
cost nothing and never block.

## Things that were fixed, and why they were not what they looked like

Each of these looked like one thing and was another. The pattern is worth
keeping in mind: **measure before believing a diagnosis.**

- **Harper "turning itself off."** `GetCursorPos().unwrap()` panicked with
  *access denied* whenever a UAC prompt, the lock screen or an elevated window
  owned the screen. That killed the highlighter process, so the toggle showed as
  off because it *was* off. Now the frame is skipped.

- **Fixes taking seconds to apply.** The cost was per injected `INPUT` event,
  about 230–312ms each, which matches Windows' 300ms `LowLevelHooksTimeout`
  default — some other app on the machine has a slow low-level keyboard hook.
  Harper cannot remove it, only send fewer events. Pasting instead of typing cut
  2N events to 4; `send` went from ~4,700ms to 19–26ms regardless of length.
  Two earlier theories (that typing was not the bottleneck, then that the lint
  loop contended with typing) were both disproved by measurement.

- **Underlines left behind by a moved window.** Positions come from reads, which
  are slow and pause during a drag. There *was* a guard, but it compared the
  move-loop's window against the one accessibility reports as focused — usually
  an inner control, so it never matched. `GetWindowRect` is now polled every
  frame and the underlines are shifted to match. A resize is deliberately *not*
  carried, because it reflows the text.

- **The tray icon "missing."** It is drawn for the macOS menu bar: of 4,213
  opaque pixels, zero are dark. On a light Windows taskbar it is white on white.
  Only the colour is replaced, never the alpha, because the alpha is what draws
  the glasses.

- **"Accept all" rewriting the whole document.** The panel is opened from a
  selection and lists only what falls inside it, but the apply path reached for
  every lint in the field. It now gathers edits from the rows on screen.

- **Selecting text stopping opening the panel.** `set_selection` compared the
  whole `Selection`, anchor included. The anchor is only *where* the selection
  sits on screen, so a moved window read as a brand new selection and threw away
  the review the user had just asked for. It now compares `start`/`len` only.

- **A lint found and never drawn, and a fix that selects the wrong words.**
  Both came through `text_range_for_span`. A span counts characters in the text
  Harper linted; the accessibility API moves endpoints in units of its own, and
  the two disagree over a `
` the app moves as one unit and over anything
  outside the basic plane, which counts twice. Nothing reports the difference —
  the range builds and lands elsewhere. Ranges are now checked against the text
  they were built to cover and re-anchored. Drawing only pays for that when the
  first attempt gives nowhere to draw, because every question runs on the
  focused app's UI thread.

## Known open problems

- **The re-anchoring above is a strong hypothesis, not a confirmed cause.** It
  explains both symptoms and is safe either way, but confirmation is a log line:
  `The app counts characters differently, re-anchored`. If instead the log says
  `The app gave nowhere to draw`, the text was scrolled out of view or the
  provider will not place it, and something else is needed.
- **Tab is only armed as a whole-field apply**, not while the review button or
  panel is showing. Offered twice, never chosen.
- **`MAX_HOVER_WORDS` is 4**, not the single word the user asked for, so that
  `to loose` and other short fixes still answer the pointer. One number to
  change if that turns out wrong.

## Build, install, verify

```bash
export PATH="$HOME/.npm-global:$PATH"
export DISABLE_WASM_OPT=1
cd harper-desktop && pnpm tauri build
```

- `pnpm` is pinned to 10.10.0 and lives in `~/.npm-global`, so the `PATH` line
  is needed in a Bash shell.
- The build ends with `A public key has been found, but no private key`. That is
  the updater-signing step only. **Both bundles are still produced and the task
  exits 0** — it is not a failure.
- Install silently, then relaunch:
  `Harper_3.0.0_arm64-setup.exe /S`, then run
  `%LOCALAPPDATA%\Harper\harper-desktop.exe`.
- **Two** `harper-desktop` processes is correct: the Tauri app and the
  highlighter. More than two means a stale instance is still around.
- `harper-desktop/src-tauri` is *excluded* from the Cargo workspace, so
  `cargo test` and `cargo fmt` must be run from inside it.
- Run `cargo fmt` before committing — `AGENTS.md` asks for it.

## Diagnosing from the user's machine

Everything useful is in `%APPDATA%\harper-desktop\`:

| File | Holds |
| --- | --- |
| `config.json` | Dialect, per-rule config, ignored lints, which apps are enabled |
| `dictionary.txt` | Words the user added |
| `highlighter.log` | The highlighter's stderr — panics, failed applies, unplaced lints |
| `timings.log` | How long each step of an apply took |

To check whether the *engine* sees a mistake, bypass the overlay entirely:

```bash
cargo run --release -p harper-cli -- lint <file>
```

This settled the "Harper misses `dont` and `havent`" question in one command:
it does not miss them, so the fault was downstream.

## How to work on this

- **Do not drive the user's mouse or keyboard, and do not type into their
  files.** Verification happens through logs, `harper-cli`, and tests. When
  something genuinely cannot be checked without the UI, say so plainly rather
  than claiming it works.
- **Measure before diagnosing, and say so when a theory is wrong.** Several
  confident explanations in this codebase's history were wrong and were only
  caught by instrumenting.
- **A silent failure is worse than a loud one.** Several bugs here were invisible
  precisely because the code did something reasonable and said nothing.
- Prefer instrumentation that reports *once per distinct text or reason* —
  anything per-frame drowns the log.
