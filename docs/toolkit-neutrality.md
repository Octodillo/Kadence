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
| Bridge objects | `daw-app/src/*_bridge.rs`, `project.rs`, `arrange_view.rs`, `piano_roll.rs`, `paint.rs`, `qt_lists.rs`, `ffi.rs` | Qt (`cxx-qt`) |
| Views | `daw-app/qml/*.qml`, `daw-app/src/cpp/*` | Qt Quick, Kirigami, KF6 |

## What is in `daw-app` and uses no Qt

These modules import no Qt type and no bridge module (lines include their
unit tests):

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
| `demo.rs`, `paths.rs`, `double_click.rs` | 364 | Demo song, data paths, double-click timing |

`tests/song.rs` uses only these modules and would move with them.
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
2. **Audio start-up and restart** (`bridge.rs`). `start_audio` (the
   fallback order when saved settings are refused), `replacing_session`,
   `poll_audio`, and `status_error` (which error stays on show) are all
   neutral decisions; `status_error` compares raw `qenum` numbers to do it.
   They should move next to `EngineHandle`, with a neutral `ErrorContext`
   enum that the bridge maps to its `qenum`.
3. **Settings storage** (`settings.rs`). The module is neutral apart from
   `KConfigStore`, which calls KConfig through `ffi.rs`. The
   `SettingsStore` trait already separates them: `KConfigStore` moves to
   the Qt side, and a GNOME front end would implement `SettingsStore` over
   GSettings or a key file.
4. **Plugin browser folders** (`plugins_bridge.rs`). `display_folder`,
   `shown_folders`, `same_rows` and the filter construction are neutral
   helpers inside the bridge.
5. **Device and instrument lists** (`settings_bridge.rs`,
   `instruments_bridge.rs`, `plugin_editors_bridge.rs`). `midi_label`,
   `source_rows`, `choice_at` and the plugin-device lookups are neutral
   and only called from bridges.
6. **Tap tempo** (`TransportBar.qml`, `tap`). The averaging and reset
   rules are in JavaScript. They belong in `transport.rs`.
7. **Closing with unsaved work** (`Main.qml`, `hasUnsavedWork`). Stopping a
   take before asking about unsaved changes is policy; the dialogs around
   it are view code and stay.

## Leaks in the other direction

No Qt type appears in an engine API. A few names were shaped for Qt and are
fine to keep, but are worth knowing:

- Bridges pass row indices and revisions as `i32` because QML numbers are
  `int`; the neutral types use `usize` and `u64`, and the conversions
  (`instruments_bridge::revision_i32`) stay on the Qt side.
- `daw_core::plugin` documents a format id as "QML's `formatId`"; the id
  itself is neutral.
- `daw_arrange::Palette` names Breeze colours as defaults for tests and
  headless rendering. A GNOME front end passes its own theme's colours.

## User-facing words

Messages reach the view as a structured `message::Fields` (context, reason,
file, numbers), and the sentences are written in QML with KI18n. That keeps
translation in the toolkit's own system, but every sentence would be
written again for GNOME, and so would the number formatting rules in
`ValueText.qml` (rounding, when Hz becomes kHz, the silence threshold).

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
- **Input**: `arrange_view.rs` and `piano_roll.rs` turn Qt pointer events
  into `daw_arrange::Pointer`. Double-click timing is already neutral
  (`double_click.rs`).
- **Timers**: QML's `UiClock` is the only timer and calls the bridges'
  ticks. GTK would use a frame clock tick callback. Once the frame tick is
  `AppState::tick` (item 1), a front end only calls it.
- **Plugin editors** open in their own X11 windows through
  `daw-plugin-window`, which is toolkit-neutral.

## The move, in order

Each step is its own pull request, and each leaves the app working.

1. **This audit.**
2. **Cleanup inside `daw-app`** (items 1 to 7 above): move the neutral logic
   out of bridges and QML into the neutral modules, with tests; move
   `KConfigStore` next to `ffi.rs`. No behaviour change.
3. **Create `daw-frontend`** and move the neutral modules and their tests
   into it with `git mv`, so history follows. `daw-app` depends on it and
   re-exports nothing; bridges import from `daw_frontend`. This is done
   once the open pull requests that touch `lanes.rs`, `message.rs`,
   `engine.rs` and `project.rs` have merged, so they do not have to follow
   a rename.
4. **Guard the line**: a CI check that `daw-frontend` builds and tests
   without Qt and that no crate except `daw-app` depends on `cxx-qt`, and
   `daw-frontend` added to the cloud test command, so its tests run where
   Qt is missing.

After that a GNOME front end is a new crate beside `daw-app` that depends on
`daw-frontend`, implements `SettingsStore`, paints `Scene`, and words
`Fields`. It is not planned yet.
