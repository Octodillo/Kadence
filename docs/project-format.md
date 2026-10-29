# Project format

Kadence projects are saved as a bundle: a directory that holds the project file
and everything it refers to. The `daw-model` crate reads and writes it
(`daw_model::Bundle`, `daw_model::bundle::{to_json, from_json}`).

## Bundle layout

```text
My Song/                      bundle directory
  My Song.kadence             project file (JSON), MIME application/x-kadence-project
  My Song.kadence.autosave    autosave, present only if there are unsaved changes
  audio/                      recorded and imported audio, plus peak caches
  backups/                    the previous 10 saves
    My Song-0001761766000123.kadence
```

- The project file is named after the bundle directory when the bundle is
  created. Opening accepts either the directory or the `.kadence` file.
  The extension is matched without regard to case. Backups are not bundles:
  read them with `bundle::read_project` and save the result into a bundle.
  Opening the autosave path, or a bundle whose project file is missing but
  whose autosave remains, resolves to the bundle so `Document::recover` works.
- A bundle directory holds exactly one `.kadence` file; creating a bundle in a
  directory that already has one fails.
- Saving copies the current project file into `backups/` (named with a
  zero-padded millisecond Unix timestamp, so name order is time order), keeps
  the newest 10, then writes the new file atomically: write `*.tmp`, fsync,
  rename, fsync the directory. A crash never leaves a half-written project.
- Autosave writes `<name>.kadence.autosave` the same atomic way and never
  touches the project file. A successful save deletes it. If it is newer than
  the project file when a bundle is opened, the app should offer to recover it
  (`Bundle::has_newer_autosave`, `Document::recover`).
- `AudioClip.source` paths are relative to the bundle root (for example
  `audio/take1.wav`), always with `/` separators. Absolute paths are allowed for files left outside the
  bundle. `Bundle::import_audio` copies a file into `audio/` under a name that
  doesn't clash.

## Project file

UTF-8 JSON, pretty-printed so that projects diff well in version control.

```json
{
  "format": "kadence-project",
  "version": 1,
  "generator": "daw-model 0.1.0",
  "project": { ... }
}
```

| Field       | Meaning                                                        |
|-------------|----------------------------------------------------------------|
| `format`    | Always `kadence-project`. Anything else is rejected.           |
| `version`   | Schema version (`daw_model::SCHEMA_VERSION`, currently 1).     |
| `generator` | What wrote the file. Informational only.                       |
| `project`   | The `Project` tree below.                                      |

### Units and conventions

- Musical time is in ticks at 960 PPQ (`daw_core::time::PPQ`), as a JSON
  integer. Sample offsets are integers in samples.
- Gains are in dB, pan is -1 (left) to 1 (right), velocity is 0 to 1.
- Floating-point values are written with the shortest representation that
  reads back to the same `f64`, so save→load is bit-exact. NaN and infinity are
  never written; commands reject them.
- Every object (track, clip, note, device, send, automation lane) has a UUID
  `id` that is unique within the project and stable across saves.
- Enums are tagged: `{"type": "audio", ...}` for clip content, devices and
  automation targets; plain strings for track kinds; `"master"` or
  `{"bus": "<track id>"}` for outputs.

### Project

| Field         | Type                      | Notes                                   |
|---------------|---------------------------|-----------------------------------------|
| `name`        | string                    |                                         |
| `sample_rate` | integer                   | preferred rate; the engine may differ   |
| `tempo`       | TempoData                 |                                         |
| `master`      | Track                     | kind `master`; not in `tracks`          |
| `tracks`      | Track[]                   | display order                           |

### TempoData

`tempos`: `[{tick, bpm, ramp}]`, strictly increasing, first at tick 0. `ramp`
means a linear change to the next point. `meters`:
`[{tick, numerator, denominator}]`, strictly increasing, first at tick 0,
denominator a power of two.

### Track

| Field        | Type                 | Notes                                          |
|--------------|----------------------|------------------------------------------------|
| `id`         | uuid                 |                                                |
| `kind`       | `audio` \| `instrument` \| `bus` \| `master` |                        |
| `name`       | string               |                                                |
| `color`      | `[r, g, b]` or null  |                                                |
| `volume_db`  | number               | fader                                          |
| `pan`        | number               |                                                |
| `mute`, `solo` | bool               |                                                |
| `output`     | `"master"` or `{"bus": id}` | target must be a bus; no loops          |
| `clips`      | Clip[]               | sorted by `start`; audio clips on audio tracks, MIDI clips on instrument tracks |
| `devices`    | Device[]             | processing order                               |
| `sends`      | Send[]               |                                                |
| `automation` | AutomationLane[]     | at most one lane per target                    |

### Clip

`{id, name, start, length, muted, content}`, with `start >= 0` and
`length > 0`. `content` is one of:

- `{"type": "audio", "source": path, "offset": samples, "gain_db": number}`
- `{"type": "midi", "notes": Note[]}`. Notes are
  `{id, start, length, channel, key, velocity}` with times relative to the clip
  start, sorted by `(start, key, id)`.

### Device

`{id, name, enabled, source, params, state}`.

- `source` is `{"type": "builtin", "kind": "<catalog id>"}` or
  `{"type": "plugin", "format": "Clap" | "Lv2" | "Vst3", "id": "<PluginDescriptor id>"}`.
- `params` maps parameter IDs (as JSON object keys) to plain values. Only
  values that differ from the default need to be present.
- `state` is the plugin's own state blob, base64, or null.

### Send

`{id, target, level_db, pre_fader}`. `target` must be a bus or the master and
must not create a routing loop.

### AutomationLane

`{id, target, enabled, points}`. `target` is one of `{"type": "volume"}`,
`{"type": "pan"}`, `{"type": "send_level", "send": id}` or
`{"type": "device_param", "device": id, "param": n}`, and must refer to a send
or device on the same track. `points` are `{tick, value, curve, step}`,
strictly increasing in tick. `curve` bends the segment to the next point
(0 is linear); `step` holds the value and jumps at the next point.

## Validation

`from_json` checks the whole tree after parsing (`Project::validate`): unique
IDs, valid ranges, sorted lists, clip and track kinds matching, routing targets
existing and loop-free, automation targets on their own track. A file that
fails is rejected rather than half-loaded.

## Versioning and migration

- `SCHEMA_VERSION` goes up by one for any change an older build couldn't read
  correctly. Adding an optional field with a default (`#[serde(default)]`)
  does not need a bump.
- Each bump appends one function to `daw_model::bundle::MIGRATIONS`;
  `MIGRATIONS[i]` turns the `project` JSON of schema `i + 1` into `i + 2`.
  Loading runs every step from the file's version up to the current one, then
  deserializes. Files at the current version skip the migration path.
- A file with a newer version than the build supports is rejected with
  `FormatError::TooNew`, never silently downgraded.
