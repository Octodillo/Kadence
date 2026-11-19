# Toolkit neutrality

Kadence's Plasma app is one front end. A GNOME one (GTK 4 and libadwaita)
may follow, on the same engine. This page says where the line between the
two runs today, what is on the wrong side of it, and the order in which it
moves. It was written from `main` at `cd02d50` (2026-11-19).

## Summary

Nothing below the app knows about Qt. The engine crates (`daw-core`,
`daw-model`, `daw-engine`, `daw-session`, `daw-arrange`, `daw-pianoroll`,
the plugin and backend crates) have no Qt dependency, and their APIs take
and return plain Rust types. The arrange view and piano roll already hand
the front end a toolkit-neutral `daw_arrange::Scene` to paint, and take
neutral `Pointer` input.

The problem is one level up. About 12,000 lines of application logic that
use no Qt type at all live inside `daw-app`, the Qt crate: the app state,
the audio start-up policy, the mixer, automation lanes, the device panel,
tracks, transport, recording, export, recovery and settings. A second front
end cannot depend on `daw-app`, since that crate cannot be built without Qt
6, KDE Frameworks and `qmake` (`build.rs`). Every one of those modules would
have to be copied. They also cannot be tested where Qt is missing, which is
why `tests/song.rs`, the end-to-end song test, lives in `daw-app` even
though "No Qt runs here".

So the plan is to move that layer into a new toolkit-neutral crate,
`daw-frontend`, and leave `daw-app` as the Qt binding: `cxx-qt` objects
that turn its state into Qt types and back, the QML, and the C++ start-up.

## The layers

| Layer | Crates | Toolkit |
|---|---|---|
| Engine, model, plugins, audio | `daw-core`, `daw-model`, `daw-engine`, `daw-session`, `daw-backend`, `daw-devices`, `daw-media`, `daw-midi`, `daw-plugin*` | none |
| Editors (scene + input) | `daw-arrange`, `daw-pianoroll` | none |
| Application logic | inside `daw-app` today; `daw-frontend` after the move | none, but built with Qt |
| Bridge objects | `daw-app/src/`: `bridge.rs`, `*_bridge.rs`, `project.rs`, `arrange_view.rs`, `piano_roll.rs`, `test_helper.rs`, `paint.rs`, `qt_lists.rs`, `ffi.rs`, `lib.rs` | Qt (`cxx-qt`; `ffi.rs` is plain `cxx`) |
| Views | `daw-app/qml/*.qml`, `daw-app/src/cpp/*` | Qt Quick, Kirigami, KF6 |

## What is in `daw-app` and uses no Qt

These modules import no Qt type and no bridge module, except that
`settings.rs` reaches KConfig through `ffi.rs` (see item 3 below). Lines
include their unit tests:

| Module | Lines | What it is |
|---|---|---|
| `state.rs`, `state/tests.rs` | 1,859 | `AppState`: the open document, engine, views, autosave, recovery, plugin catalog; the GUI-thread `with` |
| `session_mixer.rs`, `session_mixer/automation_write.rs` | 2,035 | `AppMixer`: strips, sends, inserts, touch-write automation |
| `device_panel.rs` | 1,394 | The device panel's parameters and their values |
| `lanes.rs` | 1,072 | Automation lane modes, parameter names and ranges |
| `engine.rs` | 869 | `EngineHandle`, `SessionEngine`, `StubEngine`, plugin loading |
| `settings.rs` | 672 | Settings and their stores (except `KConfigStore`, see below) |
| `message.rs` | 664 | `Message` and its `Fields` (context, reason, arguments) |
| `recording.rs` | 611 | MIDI and audio takes |
| `recovery.rs` | 550 | Scratch folders, locks, recovery candidates |
| `instrument.rs` | 520 | The instrument choices |
| `tracks.rs` | 503 | Track rows and their edits |
| `export.rs` | 491 | Export Song |
| `transport.rs` | 267 | Play, stop, record, tempo, count-in |
| `mixer.rs` | 231 | Strip ids, kinds, meter ballistics |
| `demo.rs`, `paths.rs`, `double_click.rs` | 364 | Demo song, data paths, undoing a slider's clicks when a double-click resets it |

`tests/song.rs` uses only these modules and would move with them.
`run_plugin_child` in `lib.rs` uses no Qt either, and every front end needs
it: the neutral code starts plugin hosts and scans by running the current
executable again (`HostLauncher::current_exe`, `Scanner::current_exe`), so
the binary must answer `--plugin-host` and `--scan` before anything else.
`tests/edit_names.rs` and `tests/device_names.rs` check the QML translation
tables and stay.

## Logic that sits in Qt-bound code

These are the places where a second front end would have to write the same
decision again, because it is made in a bridge object or in QML rather
than in the neutral layer.

1. **The frame tick** (`project.rs`, `Project::tick`). The order in which
   each frame ticks the engine, mixer and plugins, resyncs a refused
   update, polls peaks, routes live MIDI, follows the playhead, polls the
   recording and autosaves is app policy written inside a `cxx-qt`
   invokable. It belongs in `AppState::tick`, with the bridge only
   refreshing its properties after it.
2. **The beat's schedule** (`qml/UiClock.qml`). Each beat QML calls
   `Project.tick`, `Transport.sync`, `Plugins.tick` and, while one runs,
   `Exporter.poll`; every few beats `Engine.poll` and then
   `AppSettings.refreshMidi`, in that order so MIDI ports are made on the
   audio left after a restart. Meters tick from `MeterClock.qml`.
   `Plugins.tick` also calls `tick_plugins` a second time after
   `Project::tick` has. The schedule belongs in the neutral layer, which
   says each beat what changed; a front end only refreshes what it shows.
3. **Audio start-up and restart** (`bridge.rs`). `start_audio` (the
   fallback order when saved settings are refused), `replacing_session`,
   `poll_audio`, and `status_error` (which error stays on show) are all
   neutral decisions; `status_error` compares raw `qenum` numbers to do it.
   They should move next to `EngineHandle`, with a neutral enum for the
   context (named apart from the bridge's `ErrorContext` qenum) that the
   bridge maps to its `qenum`.
4. **Settings storage** (`settings.rs`). The module is neutral apart from
   `KConfigStore`, which calls KConfig through `ffi.rs`. The
   `SettingsStore` trait already separates them: `KConfigStore` moves to
   the Qt side, and a GNOME front end would implement `SettingsStore` over
   GSettings or a key file.
5. **Plugin browser folders** (`plugins_bridge.rs`). `display_folder`,
   `shown_folders`, `home_folder`, `same_rows`, `format_at` and the filter
   construction (`filter_for`) are decisions inside the bridge.
6. **Device and instrument lists** (`settings_bridge.rs`,
   `instruments_bridge.rs`, `plugin_editors_bridge.rs`). `midi_label`,
   `source_rows`, `choice_at` and the plugin-device lookups are only called
   from bridges.
7. **The transport display** (`transport_bridge.rs`, `Shown::of`): whether
   bar and beat come from the engine or the document, and the "bar.beat"
   and "m:ss.s" texts. Also the window title's fallback from the project's
   name to the bundle's (`project.rs`, `shown_name`).

   Some of these are Qt-shaped today: `filter_for` takes a `qenum`, the
   editor lookup takes a `QString` (and repeats the device panel's), and
   `choice_at` takes `i32` rows and revisions. They move with neutral
   signatures (`usize`, `u64`, `&str`, a Rust enum) and the bridge
   converts.
8. **Tap tempo** (`TransportBar.qml`, `tap`). The averaging and reset
   rules are in JavaScript. They belong in `transport.rs`.
9. **Closing with unsaved work** (`Main.qml`, `hasUnsavedWork`). Stopping a
   take before asking about unsaved changes is policy; the dialogs around
   it are view code and stay.

## Leaks in the other direction

No Qt type appears in an engine API. A few names were shaped for Qt and are
fine to keep, but are worth knowing:

- Bridges pass row indices and revisions as `i32` because QML numbers are
  `int`; the neutral types use `usize` and `u64`, and the conversions
  (`instruments_bridge::revision_i32`) belong on the Qt side (see item 6).
- `daw_core::plugin` documents a format id as "QML's `formatId`"; the id
  itself is neutral.
- `daw_arrange::Palette` names Breeze colours as defaults for tests and
  headless rendering. A GNOME front end passes its own theme's colours.

## User-facing words

Messages reach the view as a structured `message::Fields` (context, reason,
file, numbers), and the sentences are written in QML with KI18n. That keeps
translation in the toolkit's own system, but every sentence would be
written again for GNOME, and so would the number formatting rules in
`ValueText.qml` (rounding, when Hz becomes kHz, how silence is worded;
the silence threshold itself comes from Rust).

Edit names in the undo history and built-in device names work the same way:
English keys in Rust, translated by tables in `EditNames.qml` and
`DeviceNames.qml`.

That is a choice to make when a GNOME front end starts, not now: either
each front end keeps its own wording, or wording moves into Rust behind
gettext (KI18n reads the same catalogues) and both front ends share it. The
move below keeps `Fields` exactly as it is, so either choice stays open.

## Paint, input and timers

- **Paint**: `paint.rs` and `cpp/scenepainter.cpp` turn a `Scene` into
  `QPainter` calls. A GNOME front end would do the same with GSK or Cairo
  in a `GtkWidget::snapshot`.
- **Input**: `paint.rs` (`pointer`, `cursor_shape`) turns Qt pointer
  events and cursors into `daw_arrange`'s, and the views get double-clicks
  from Qt through their `double_click` invokables.
- **Timers**: QML's `UiClock` is the only clock that polls app state (other
  QML timers are view details: the long-press menu, a debounce before
  saving a layout). GTK would use a frame clock tick callback. Once the
  beat's schedule is neutral (items 1 and 2), a front end only calls it.
- **Plugin editors** open in their own X11 windows through
  `daw-plugin-window`, which is toolkit-neutral but names Kadence's
  `WM_CLASS` itself; a front end with another app id would pass its own.

## The move, in order

Each step is its own pull request, and each leaves the app working.

1. **This audit.**
2. **Cleanup inside `daw-app`** (items 1 to 9 above, in one or two PRs):
   move the neutral logic out of bridges and QML into the neutral
   modules, with tests; move `KConfigStore` next to `ffi.rs`. No behaviour
   change.
3. **Create `daw-frontend`** and move the neutral modules, `tests/song.rs`
   and `run_plugin_child` into it with `git mv`, so history follows.
   `daw-app` depends on it and re-exports nothing; bridges import from
   `daw_frontend`, and `main.rs` calls `daw_frontend::run_plugin_child`.
   The private modules (`demo`, `paths`, `recording`, `transport`) and the
   `pub(crate)` items bridges use become public; the `#[cfg(test)]` hooks
   that bridge tests use (`StubEngine`'s, `AppState::with_engine`) go
   behind a `test-support` feature. Doc links to `crate::run_plugin_child`
   and `crate::tracks_bridge` are rewritten. This is done once the open
   pull requests that touch `lanes.rs`, `message.rs`, `engine.rs` and
   `project.rs` have merged, so they do not have to follow a rename.
4. **Guard the line**: a CI check that no crate except `daw-app` depends on
   `cxx-qt` or `cxx`. The fast job's `--workspace --exclude daw-app` then
   builds and tests `daw-frontend` without Qt as it is. The Arch job's
   song-test filters (`package(daw-app) and binary(song)`) move to
   `daw-frontend`, and the fast job leaves the song test to that step.

After that a GNOME front end is a new crate beside `daw-app` that depends on
`daw-frontend`, implements `SettingsStore`, paints `Scene`, runs the beat,
and words `Fields`. It is not planned yet.
