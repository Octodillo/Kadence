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
  backups/                    the 10 versions before the current one
    My Song-0001761766000123.kadence
```

- The project file is named after the bundle directory when the bundle is
  created. Opening accepts either the directory or the `.kadence` file.
  The extension is matched without regard to case. Backups are not bundles:
  read them with `bundle::read_project` and save the result into a bundle.
  Opening the autosave path, or a bundle whose project file is missing but
  whose autosave remains, resolves to the bundle so `Document::recover` works.
- A bundle directory holds exactly one `.kadence` file; creating a bundle in a
  directory that already has one, or has a `.kadence.autosave`, fails.
- Save As takes the bundle directory or, as a file dialog gives it, the
  project file `Name.kadence` for the directory `Name` around it. Choosing
  the open project's own bundle, by its folder or a file in it, is a plain
  save; choosing another project's file fails rather than write into its
  bundle. It copies the old bundle's `audio/` folder with its sub-folders,
  and any other file in the old bundle that a clip plays (never a project
  file, an autosave or anything in `backups/`), each to the same place in
  the new bundle, and fails rather than overwrite a file already there. A
  failed Save As removes what it copied.
- Saving copies the current project file into `backups/` as
  `<name>-<timestamp>.kadence`, where the timestamp is a zero-padded
  millisecond Unix time, always later than the newest backup's, then writes
  the new file atomically: write a new `.kadence-<pid>-<n>.tmp` beside it,
  fsync, rename, fsync the directory (`daw_core::atomic_file`, which exports
  and caches use too). A temporary file a killed Kadence left behind is
  removed by a later write into the same folder once it is an hour old. A
  crash never leaves a half-written project. The new file keeps the old
  one's permissions. Once it is written, that project's backups beyond the
  newest 10 are removed (backups under another name, from before the project
  file was renamed, are left alone). If the write fails, the backup just
  made is removed again.
- Saving a project whose file already holds exactly what would be written
  changes nothing, so repeated saves do not push earlier versions out of
  `backups/`.
- Only creating a bundle makes its directory. If the directory of an open
  bundle was moved, renamed or deleted, saving, autosaving, importing and
  recording fail with a not-found error naming it instead of starting a
  second bundle at the old place. So does Save As when clips play files of
  the bundle, since the new bundle would be without them.
- A symlinked project file is written through, and stays a link, only when
  the open project was read from it. After recovering an autosave or Save As
  the project file was not read, so a link there is replaced by the new file
  and what it pointed to is neither backed up nor changed.
- Temporary files and backups are only ever created new, never opened if the
  name is taken, so a link at such a name in a bundle from somewhere else
  cannot direct a write to another file.
- Autosave writes `<name>.kadence.autosave` the same atomic way and never
  touches the project file. A link at that name is replaced, not followed.
  Symlinked `audio/` and `backups/` folders are still followed. Saving
  deletes it, and so does undoing back to the saved state; a save still
  succeeds if the autosave cannot be deleted. If one exists when a bundle is
  opened, `Document::open` fails with `FormatError::AutosavePending` instead
  of loading the older file: the
  app asks the user, then calls `Document::recover` (load the autosave) or
  `Document::open_discarding_autosave`. The open document therefore always owns
  its bundle's autosave. In a bundle that cannot be written, discarding
  still opens the saved version and the autosave stays to be offered again.
- `AudioClip.source` paths are relative to the bundle root (for example
  `audio/take1.wav`), always with `/` separators. Absolute paths are allowed
  for files left outside the bundle. `Bundle::import_audio` copies a file
  into `audio/` under a name that doesn't clash, and leaves nothing there if
  the copy fails.

## Project file

UTF-8 JSON, pretty-printed so that projects diff well in version control.

```json
{
  "format": "kadence-project",
  "version": 4,
  "generator": "daw-model 0.1.0",
  "project": { ... }
}
```

| Field       | Meaning                                                        |
|-------------|----------------------------------------------------------------|
| `format`    | Always `kadence-project`. Anything else is rejected.           |
| `version`   | Schema version (`daw_model::SCHEMA_VERSION`, currently 4).     |
| `generator` | What wrote the file. Informational only.                       |
| `project`   | The `Project` tree below.                                      |

### Units and conventions

- Musical time is in ticks at 960 PPQ (`daw_core::time::PPQ`), as a JSON
  integer. Sample offsets are integers in samples. No position or length
  may be past tick 2^40 (`MAX_PROJECT_TICK`, about 18 years at 120 BPM), and no
  audio offset past sample 2^48; a file that says otherwise does not load.
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
| `name`        | string                    | empty: the UI shows the bundle name, or "Untitled" |
| `sample_rate` | integer                   | preferred rate; the engine may differ   |
| `tempo`       | TempoData                 |                                         |
| `master`      | Track                     | kind `master`; not in `tracks`          |
| `tracks`      | Track[]                   | display order                           |

### TempoData

`tempos`: `[{tick, bpm, ramp}]`, strictly increasing, first at tick 0, `bpm`
from 1 to 100000. `ramp` means a linear change to the next point. `meters`:
`[{tick, numerator, denominator}]`, strictly increasing, first at tick 0,
denominator a power of two.

### Track

| Field        | Type                 | Notes                                          |
|--------------|----------------------|------------------------------------------------|
| `id`         | uuid                 |                                                |
| `kind`       | `audio` \| `instrument` \| `bus` \| `master` |                        |
| `name`       | string               | the master's is empty unless renamed; the UI words it |
| `color`      | `[r, g, b]` or null  |                                                |
| `volume_db`  | number               | fader                                          |
| `pan`        | number               |                                                |
| `mute`, `solo` | bool               |                                                |
| `armed`      | bool                 | optional, default false; armed for recording (audio and instrument tracks only) |
| `audio_input` | `{channels, monitor}` | optional; audio tracks only. `channels` is `{"mono": n}` or `{"stereo": n}` (inputs `n`, `n+1`), counted from 0, default `{"mono": 0}`; `monitor` is `"off"` (default) or `"auto"` |
| `midi_input` | `{device, channel}`  | live MIDI an instrument track takes: `device` is a source port name (`client:port`) or `null` for all; `channel` is 0–15 or `null` for all. Default: all |
| `output`     | `"master"` or `{"bus": id}` | target must be a bus; no loops          |
| `clips`      | Clip[]               | sorted by `start`; audio clips on audio tracks, MIDI clips on instrument tracks |
| `devices`    | Device[]             | processing order                               |
| `sends`      | Send[]               |                                                |
| `automation` | AutomationLane[]     | at most one lane per target                    |

### Clip

`{id, name, start, length, muted, content}`, with `start >= 0` and
`length > 0`. `content` is one of:

- `{"type": "audio", "source": path, "offset": samples, "gain_db": number,
  "fade_in": Fade, "fade_out": Fade}`. A Fade is `{length, shape}`: `length`
  in ticks (0 is no fade, never negative) and `shape` one of `linear`,
  `equal_power` (quarter sine) or `s_curve` (raised cosine). Both default to
  no fade. If the two lengths add up to more than the clip (after a trim),
  playback shortens both in proportion.
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
  or write back correctly. That includes adding a field, even an optional one
  with a default (`#[serde(default)]`): an older build ignores fields it
  doesn't know and would drop them on its next save, so it must reject the
  file with `TooNew` instead. Give the new field a default and its
  migration can return the JSON unchanged.
- Each bump appends one function to `daw_model::bundle::MIGRATIONS`;
  `MIGRATIONS[i]` turns the `project` JSON of schema `i + 1` into `i + 2`.
  Loading runs every step from the file's version up to the current one, then
  deserializes. Files at the current version skip the migration path.
- A file with a newer version than the build supports is rejected with
  `FormatError::TooNew`, never silently downgraded.
- `crates/daw-model/tests/schema/v<N>.keys` lists every key a version `N`
  file can hold, and a test compares it with what the build writes. When it
  fails, a field was added or renamed: raise the version and add the new
  list instead of editing the old one.

### History

| Version | Change |
|---------|--------|
| 1       | First schema. |
| 2       | Default names are no longer saved in English: a project name of `"Untitled"` and a master named `"Master"` become `""`, and the UI words them in the user's language. |
| 3       | Audio clips get `fade_in` and `fade_out`. Missing fades mean no fade, so version 2 files load unchanged. Tracks later gained `armed` and `audio_input` without a version of their own, so a version 3 file may or may not hold them; missing means not armed and the first input, mono, monitoring off. |
| 4       | Instrument tracks gain `midi_input` (`device`: a source port name or `null` for every input; `channel`: 0–15 or `null` for every channel). Missing means every input, so version 3 files load unchanged. |
