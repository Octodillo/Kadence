# User stories for Kadence 0.1: pain points and feature gaps

November 2026, against `main` at the 0.1.0 release.

Nobody had looked at Kadence from a user's side before this. Seven personas
were each walked through their real workflow, from first launch to a finished
file, by reading what the 0.1.0 interface can actually do (the QML, the app
bridges, the model's commands and the user guide). Every "Kadence can't do
this" claim was then handed to a second reader whose job was to refute it by
finding the feature somewhere else. This is a code walkthrough, not a study
with real users or a running application, so it finds missing and awkward
paths well and says little about feel, performance or how often real people
would hit each one.

## Summary

- 78 user stories, 219 findings before merging, 47 themes after.
- The fact-check confirmed 205 findings, corrected details in 13 and refuted 1
  (automation write also records device-panel moves). It also raised or
  lowered several effort estimates; the table uses the corrected ones.
- Outcomes of the 78 stories: 7 work, 29 work awkwardly, 22 are partly missing, 18 are missing, 2 break.
- The widest gaps are everyday editing basics rather than big features:
  **copy and paste** (all seven personas), a **grid chooser and snap toggle**
  (all seven), **transport keys**, and four small things the model already
  supports but the interface does not offer (**clip mute, clip rename, track
  colour, Duplicate Track**).
- A recurring pattern: the engine or model already does it and only the
  interface is missing. That holds for the grid, tempo changes and ramps, clip
  mute and rename, track colour, the Sampler's samples, velocity tools,
  metronome options and the demo song. These are the cheapest wins.
- Three personas cannot finish their core job today: the **podcaster** (no
  time ruler, ripple editing, loudness or MP3), the **film composer** (no
  tempo changes, markers, controllers or stems) and the **touch user** (no
  two-finger pan, no piano roll touch handling, no way to play notes).

## How to read the priority table

- **Impact** is 1 to 5: 5 blocks a persona's core workflow, 1 is a nicety. It
  is the highest rating any persona gave the theme.
- **Reach** is how many of the seven personas hit it.
- **Effort** is S (under a day), M (a few days), L (one to two weeks), XL (a
  major subsystem), weighted 1, 2, 4 and 8.
- **Score** is impact × reach ÷ effort weight. It is a sorting aid, not a
  verdict: a theme that blocks one persona completely (T14, T21, T33) scores
  low on reach and may still be the right thing to do next for that audience.

Personas: BP bedroom producer, SS singer-songwriter, BM beatmaker, POD
podcaster, FC film composer, LM Linux switcher, SD Steam Deck / touch.

| Rank | Theme | Impact | Reach | Effort | Score | Issue |
| ---: | --- | :---: | --- | :---: | ---: | --- |
| 1 | T04 Clip mute, clip rename, track colour, Duplicate Track | 4 | 6: BP SS BM POD FC LM | S | 24 | #189 |
| 2 | T05 Transport navigation and track shortcuts | 4 | 6: BP SS POD FC LM SD | S | 24 | #191 |
| 3 | T02 Cut, copy and paste for clips, notes and automation | 5 | 7: BP SS BM POD FC LM SD | M | 17.5 | #185 |
| 4 | T08 MP3 / Ogg export with metadata tags | 5 | 6: BP SS BM POD FC LM | M | 15 | #194 |
| 5 | T03 Grid chooser, triplets, swing and a snap toggle | 4 | 7: BP SS BM POD FC LM SD | M | 14 | #188 |
| 6 | T10 Markers for sections, hit points and chapters | 5 | 5: BP BM POD FC LM | M | 12.5 | #197 |
| 7 | T42 Export dialog conveniences | 3 | 4: SS POD FC SD | S | 12 |  |
| 8 | T12 Standard MIDI File import and export | 5 | 4: BP BM FC LM | M | 10 | #200 |
| 9 | T27 Keyboard shortcut list and editor | 4 | 5: BP BM POD LM SD | M | 10 |  |
| 10 | T32 Recordings made before the first save stay outside the project | 4 | 2: SS POD | S | 8 |  |
| 11 | T37 Audio editing aids | 4 | 4: BP SS BM POD | M | 8 |  |
| 12 | T44 User guide corrections | 2 | 4: SS BM POD LM | S | 8 |  |
| 13 | T06 Input level meter, visible monitoring, named inputs | 5 | 3: SS POD SD | M | 7.5 | #192 |
| 14 | T09 Loudness: LUFS meter, limiter, normalised export | 5 | 3: BP SS POD | M | 7.5 | #195 |
| 15 | T11 Stem export | 5 | 6: BP SS BM POD FC LM | L | 7.5 | #198 |
| 16 | T13 Tempo and time-signature changes on the timeline | 5 | 3: BP FC LM | M | 7.5 | #201 |
| 17 | T16 Takes: loop recording, punch in/out, record-over | 5 | 5: BP SS POD FC LM | L | 6.25 | #205 |
| 18 | T25 Presets, and copying devices between tracks | 4 | 6: BP SS BM POD FC LM | L | 6 |  |
| 19 | T26 Project templates | 4 | 3: BP POD FC | M | 6 |  |
| 20 | T30 Piano roll entry: draw tool, note defaults, drum grid | 4 | 3: BP BM SD | M | 6 |  |
| 21 | T31 DSP load and xrun indicator | 3 | 4: BP FC LM SD | M | 6 |  |
| 22 | T40 Keep audio in place when the tempo changes | 4 | 3: POD FC LM | M | 6 |  |
| 23 | T01 Load samples into the built-in Sampler | 5 | 2: BP BM | M | 5 | #178 |
| 24 | T14 Minutes:seconds timeline and deeper zoom-out | 5 | 2: POD FC | M | 5 | #202 |
| 25 | T17 Time-range selection and ripple editing | 5 | 4: BP POD FC LM | L | 5 | #207 |
| 26 | T19 Sidechain routing (ducking, pumping) | 5 | 4: BP BM POD LM | L | 5 |  |
| 27 | T07 Growing clip and elapsed time while recording | 4 | 2: SS POD | M | 4 | #192 |
| 28 | T15 Open a project from the command line and Dolphin | 4 | 1: LM | S | 4 | #203 |
| 29 | T28 Plugin browser: favourites, recents, categories, blocklist | 4 | 2: BM LM | M | 4 |  |
| 30 | T41 Automation discoverability | 2 | 2: BM FC | S | 4 |  |
| 31 | T43 First-run path to a first sound | 2 | 2: BP BM | S | 4 |  |
| 32 | T22 Play notes without MIDI hardware | 5 | 3: BP BM SD | L | 3.75 |  |
| 33 | T39 More built-in devices | 3 | 4: BP SS BM POD | L | 3 |  |
| 34 | T46 Recording latency offset | 3 | 2: LM SS | M | 3 |  |
| 35 | T18 MIDI controllers: sustain, pitch bend, CC lanes | 5 | 4: BP BM FC LM | XL | 2.5 | #208 |
| 36 | T21 Touch navigation and no-keyboard paths | 5 | 1: SD | M | 2.5 | #209 |
| 37 | T23 Track folders, hide/search, multi-select | 5 | 2: BP FC | L | 2.5 |  |
| 38 | T29 More than two hardware inputs and outputs | 5 | 2: LM SS | L | 2.5 |  |
| 39 | T24 Freeze / bounce track to audio | 4 | 2: BP FC | L | 2 |  |
| 40 | T38 Import gaps for long and phone-recorded files | 4 | 1: POD | M | 2 |  |
| 41 | T20 Time-stretch, warp and pitch-shift for audio | 5 | 3: BP BM LM | XL | 1.875 |  |
| 42 | T34 Multi-output and multi-timbral instruments | 4 | 3: BM FC LM | XL | 1.5 |  |
| 43 | T45 MIDI learn and controller mapping | 3 | 2: BM LM | L | 1.5 |  |
| 44 | T47 Mixer on touch and small screens | 3 | 1: SD | M | 1.5 |  |
| 45 | T35 External sync and MIDI out (JACK transport, MIDI clock, NSM) | 4 | 1: LM | L | 1 |  |
| 46 | T36 Process the graph on more than one core | 4 | 1: FC | L | 1 |  |
| 47 | T33 Video playback and timecode | 5 | 1: FC | XL | 0.625 |  |

## Suggested order of work

1. **Editing basics, one or two threads, all small or medium.** T04, T05, T03
   and T02. These touch every persona and mostly wire up what exists. T44
   (guide fixes) rides along with T05.
2. **Finish a song and get it out.** T08 (MP3/Ogg), T09 step 1 (peak and
   loudness report on export), T42, then T11 (stems). T01 (Sampler samples,
   #178) belongs here too because sample drums are impossible without it.
3. **Recording confidence.** T06 and T07, T32, then the first two steps of
   T16 (mute what a take covers; cut loop passes into takes).
4. **Structure.** T10 (markers), T13 (tempo changes), T12 (MIDI files), T26
   (templates).
5. **Pick an audience for the next larger bet.** Spoken word needs T14, T17
   and T09. Scoring needs T13, T10, T18 and T11. Beat making needs T01, T30,
   T19 and T20. Touch needs T21 and T22. These do not compete for the same
   files much, but each is a week or more.

Schema changes cluster: markers (T10), controllers (T18), loop range saved in
the project (T05), folders (T23) and time-locked audio (T40) all bump the
project format, so they should be sequenced, not run in parallel.

## Themes

### T01 Load samples into the built-in Sampler

Impact 5, effort M, score 5. Issue #178. Hit by: Bedroom producer, Beatmaker.

The Sampler is offered as a built-in instrument and named in the guide, but nothing in the UI can give it a sample, so choosing it replaces the working synth with silence. The engine side (zones, one-shot and key-mapped modes, state) is complete. Already filed by the code review as part of #178; listed here because it blocks sample-based drums entirely. A drum-kit view (one sample per pad) is the follow-up.

### T02 Cut, copy and paste for clips, notes and automation

Impact 5, effort M, score 17.5. Issue #185. Hit by: Bedroom producer, Singer-songwriter, Beatmaker, Podcaster, Film composer, Linux switcher, Steam Deck / touch.

Only Duplicate exists, which puts the copy right after the selection. No copy to another bar, track or clip, no Ctrl+drag copy. Ids and undo batches were designed with paste in mind.

### T03 Grid chooser, triplets, swing and a snap toggle

Impact 4, effort M, score 14. Issue #188. Hit by: Bedroom producer, Singer-songwriter, Beatmaker, Podcaster, Film composer, Linux switcher, Steam Deck / touch.

The grid is always adaptive and never shown; Quantize depends on zoom; triplets and swing are unreachable; snap can only be bypassed per drag with a modifier that differs between pages. `Snap::Fixed`, `Snap::Off`, triplet/dotted feel and swing already exist below the UI.

### T04 Clip mute, clip rename, track colour, Duplicate Track

Impact 4, effort S, score 24. Issue #189. Hit by: Bedroom producer, Singer-songwriter, Beatmaker, Podcaster, Film composer, Linux switcher.

All four have model support and no UI (Duplicate Track is medium because ids must be regenerated).

### T05 Transport navigation and track shortcuts

Impact 4, effort S, score 24. Issue #191. Hit by: Bedroom producer, Singer-songwriter, Podcaster, Film composer, Linux switcher, Steam Deck / touch.

Home is the only locate key; Stop does not return; no go to end, bar stepping, or keys for mute/solo/arm/metronome (the guide wrongly implies M and S work); follow-playhead cannot be turned off; the loop range is discarded when Loop is switched off.

### T06 Input level meter, visible monitoring, named inputs

Impact 5, effort M, score 7.5. Issue #192. Hit by: Singer-songwriter, Podcaster, Steam Deck / touch.

An armed track shows no input level unless software monitoring is on, and then only on the Mix page. The monitoring switch is buried in a menu with no visible state.

### T07 Growing clip and elapsed time while recording

Impact 4, effort M, score 4. Issue #192. Hit by: Singer-songwriter, Podcaster.

The lane stays empty during an audio take; only the playhead colour changes. No elapsed-take or free-disk readout.

### T08 MP3 / Ogg export with metadata tags

Impact 5, effort M, score 15. Issue #194. Hit by: Bedroom producer, Singer-songwriter, Beatmaker, Podcaster, Film composer, Linux switcher.

Export is WAV and FLAC only, with no tags. The streaming writer abstraction takes a new format variant.

### T09 Loudness: LUFS meter, limiter, normalised export

Impact 5, effort M, score 7.5. Issue #195. Hit by: Bedroom producer, Singer-songwriter, Podcaster.

Peak bar only on the master, no limiter among built-ins, no loudness target, and no peak or clipping report after export.

### T10 Markers for sections, hit points and chapters

Impact 5, effort M, score 12.5. Issue #197. Hit by: Bedroom producer, Beatmaker, Podcaster, Film composer, Linux switcher.

No markers of any kind. Needs a project field and format bump; ruler gesture and drawing patterns exist.

### T11 Stem export

Impact 5, effort L, score 7.5. Issue #198. Hit by: Bedroom producer, Singer-songwriter, Beatmaker, Podcaster, Film composer, Linux switcher.

Export renders the master only. Needs taps at track and bus outputs and several writers in one offline pass.

### T12 Standard MIDI File import and export

Impact 5, effort M, score 10. Issue #200. Hit by: Bedroom producer, Beatmaker, Film composer, Linux switcher.

No .mid in or out. Model commands and a multi-point tempo map are ready; only a reader/writer is new.

### T13 Tempo and time-signature changes on the timeline

Impact 5, effort M, score 7.5. Issue #201. Hit by: Bedroom producer, Film composer, Linux switcher.

The transport fields edit the single existing point, so typing a tempo at bar 33 retimes the whole song. The model, engine, grid and export all support multiple points and ramps already.

### T14 Minutes:seconds timeline and deeper zoom-out

Impact 5, effort M, score 5. Issue #202. Hit by: Podcaster, Film composer.

Ruler and grid are bars and beats only; an hour of audio cannot fit the window because of the minimum zoom; the tempo field is easy to nudge by accident, which moves audio.

### T15 Open a project from the command line and Dolphin

Impact 4, effort S, score 4. Issue #203. Hit by: Linux switcher.

No positional argument, no MIME type on the desktop entry, no drop of a project on the window.

### T16 Takes: loop recording, punch in/out, record-over

Impact 5, effort L, score 6.25. Issue #205. Hit by: Bedroom producer, Singer-songwriter, Podcaster, Film composer, Linux switcher.

Loop recording gives one run-on clip; recording over a clip plays both at once; no punch or pre-roll.

### T17 Time-range selection and ripple editing

Impact 5, effort L, score 5. Issue #207. Hit by: Bedroom producer, Podcaster, Film composer, Linux switcher.

Edits address whole clips. No delete-and-close-gap, no insert or delete time, no split across all tracks.

### T18 MIDI controllers: sustain, pitch bend, CC lanes

Impact 5, effort XL, score 2.5. Issue #208. Hit by: Bedroom producer, Beatmaker, Film composer, Linux switcher.

MIDI clips hold notes only, so pedal, bend and mod wheel are heard live and lost on recording. Record-and-replay alone is large; lanes make it extra large.

### T19 Sidechain routing (ducking, pumping)

Impact 5, effort L, score 5. No issue filed yet. Hit by: Bedroom producer, Beatmaker, Podcaster, Linux switcher.

Neither the built-in compressor nor hosted plugins can be keyed from another track; plugin sidechain ports get silence. Needs a second audio input per device node and a model field. Related to send identity in #114.

### T20 Time-stretch, warp and pitch-shift for audio

Impact 5, effort XL, score 1.875. No issue filed yet. Hit by: Bedroom producer, Beatmaker, Linux switcher.

Loops do not follow the project tempo; only fixed-ratio resampling exists. A stretch library (for example Rubber Band, licence permitting) and a clip-level stretch ratio would be the start.

### T21 Touch navigation and no-keyboard paths

Impact 5, effort M, score 2.5. Issue #209. Hit by: Steam Deck / touch.

Two fingers only zoom; the piano roll has no touch handling; handles are 6 to 8 px; several actions need Shift, right-click or double-tap.

### T22 Play notes without MIDI hardware

Impact 5, effort L, score 3.75. No issue filed yet. Hit by: Bedroom producer, Beatmaker, Steam Deck / touch.

No on-screen piano or pads and no computer-keyboard note input. Live MIDI already routes to the armed track, so a GUI-side virtual source is the missing piece. Computer-keyboard input alone is medium.

### T23 Track folders, hide/search, multi-select

Impact 5, effort L, score 2.5. No issue filed yet. Hit by: Bedroom producer, Film composer.

Flat track list, one selected track at a time, one global track height. Letting a bus act as a collapsible folder would reuse existing routing.

### T24 Freeze / bounce track to audio

Impact 4, effort L, score 2. No issue filed yet. Hit by: Bedroom producer, Film composer.

No way to render a heavy instrument track in place. Offline render and single-undo import exist; shares the per-track tap with stem export (T11).

### T25 Presets, and copying devices between tracks

Impact 4, effort L, score 6. No issue filed yet. Hit by: Bedroom producer, Singer-songwriter, Beatmaker, Podcaster, Film composer, Linux switcher.

No host-level presets for built-in devices or plugins (a plugin's own window can still use its own browser), no chain presets, and an effect cannot be copied or dragged to another track. State blobs and parameter maps already round-trip, so a preset is those two fields in a file.

### T26 Project templates

Impact 4, effort M, score 6. No issue filed yet. Hit by: Bedroom producer, Podcaster, Film composer.

Every project starts empty. A template is a project bundle opened as untitled; add Save as Template and a chooser on New Project.

### T27 Keyboard shortcut list and editor

Impact 4, effort M, score 10. No issue filed yet. Hit by: Bedroom producer, Beatmaker, Podcaster, Linux switcher, Steam Deck / touch.

Shortcuts are literals in QML with no in-app list and no rebinding. KDE users expect Configure Keyboard Shortcuts. A read-only list is the medium first step; a full editor is large. Would also let Deck users bind back buttons.

### T28 Plugin browser: favourites, recents, categories, blocklist

Impact 4, effort M, score 4. No issue filed yet. Hit by: Beatmaker, Linux switcher.

With hundreds of plugins there is search but no favourites, recents, categories, duplicate-format handling, or way to disable a plugin or format. Related: #118, #173.

### T29 More than two hardware inputs and outputs

Impact 5, effort L, score 2.5. No issue filed yet. Hit by: Linux switcher, Singer-songwriter.

The JACK client is fixed at stereo in and out, so an 8-input interface or a separate cue output cannot be used. More inputs is medium (the config and input menu are already N-channel); hardware outputs per track need an engine change. Also: auto-connect cannot be turned off, and changing buffer size re-runs auto-connect and discards the user's patching.

### T30 Piano roll entry: draw tool, note defaults, drum grid

Impact 4, effort M, score 6. No issue filed yet. Hit by: Bedroom producer, Beatmaker, Steam Deck / touch.

Double-click both adds and deletes a note; no pencil, paint or eraser; new-note length and velocity are fixed; velocity tools exist in the bridge with no UI; a clip cannot be lengthened from the piano roll and notes past its end are refused. Scale highlighting, ghost notes, drum names and a step grid are follow-ups.

### T31 DSP load and xrun indicator

Impact 3, effort M, score 6. No issue filed yet. Hit by: Bedroom producer, Film composer, Linux switcher, Steam Deck / touch.

No CPU or xrun readout during playback; xruns are reported once per take while recording only.

### T32 Recordings made before the first save stay outside the project

Impact 4, effort S, score 8. No issue filed yet. Hit by: Singer-songwriter, Podcaster.

Takes recorded into an untitled project stay in the scratch recordings folder after Save As, so the project folder is not self-contained until the user finds Collect Audio Files. Move them on first save. Check against #181 (orphaned takes) before filing.

### T33 Video playback and timecode

Impact 5, effort XL, score 0.625. No issue filed yet. Hit by: Film composer.

No video, timecode, frame rate or cue start offset. A timecode display and start offset alone are medium.

### T34 Multi-output and multi-timbral instruments

Impact 4, effort XL, score 1.5. No issue filed yet. Hit by: Beatmaker, Film composer, Linux switcher.

Only a plugin's main output is connected, and one instrument cannot serve several MIDI tracks. Related: #24, #77.

### T35 External sync and MIDI out (JACK transport, MIDI clock, NSM)

Impact 4, effort L, score 1. No issue filed yet. Hit by: Linux switcher.

No transport sync, no MIDI output to hardware, no session-manager support. Related: #107.

### T36 Process the graph on more than one core

Impact 4, effort L, score 1. No issue filed yet. Hit by: Film composer.

The engine runs every step in sequence. Each sandboxed plugin already has its own thread, so steps on the same dependency level could be signalled together.

### T37 Audio editing aids

Impact 4, effort M, score 8. No issue filed yet. Hit by: Bedroom producer, Singer-songwriter, Beatmaker, Podcaster.

Default short fades at cuts and take edges (related #101, #102), Normalize and numeric clip gain, split at pointer and across all tracks, clip loop/repeat by dragging, reverse, strip silence, vertical waveform zoom.

### T38 Import gaps for long and phone-recorded files

Impact 4, effort M, score 2. No issue filed yet. Hit by: Podcaster.

No M4A/AAC or Opus import; MP3/Ogg files with no stated length are refused; copying a large file into the project blocks the window; no split of a stereo file into two mono tracks.

### T39 More built-in devices

Impact 3, effort L, score 3. No issue filed yet. Hit by: Bedroom producer, Singer-songwriter, Beatmaker, Podcaster.

Gate/expander, de-esser, tuner, saturation, a standalone filter, and note effects (arpeggiator, note repeat). The limiter is in T09. Each device is small to medium; an EQ response curve is a separate medium item.

### T40 Keep audio in place when the tempo changes

Impact 4, effort M, score 6. No issue filed yet. Hit by: Podcaster, Film composer, Linux switcher.

Audio clips are positioned in ticks, so any tempo edit moves them in time. A per-track Lock to Time that re-derives positions inside the tempo edit's undo batch needs no engine change.

### T41 Automation discoverability

Impact 2, effort S, score 4. No issue filed yet. Hit by: Beatmaker, Film composer.

Write Automation is only on the Mix page although it also records device-panel moves; the add-lane menu is one flat list of every parameter; parameter rows have no Automate This action.

### T42 Export dialog conveniences

Impact 3, effort S, score 12. No issue filed yet. Hit by: Singer-songwriter, Podcaster, Film composer, Steam Deck / touch.

No suggested file name, last choices not remembered, range in whole bars only, stereo only (mono export is small), nothing to do with the file afterwards (Open Containing Folder).

### T43 First-run path to a first sound

Impact 2, effort S, score 4. No issue filed yet. Hit by: Bedroom producer, Beatmaker.

The empty-state placeholder offers only Add Audio Track; the demo song builder is not reachable from the UI; nothing hints that double-click adds a clip.

### T44 User guide corrections

Impact 2, effort S, score 8. No issue filed yet. Hit by: Singer-songwriter, Beatmaker, Podcaster, Linux switcher.

The guide says M and S mute and solo (they do not); it omits monitoring, the Shift snap bypass in Arrange, and half the piano-roll features; there is no spoken-word path.

### T45 MIDI learn and controller mapping

Impact 3, effort L, score 1.5. No issue filed yet. Hit by: Beatmaker, Linux switcher.

Hardware knobs, faders and transport buttons cannot be bound to anything.

### T46 Recording latency offset

Impact 3, effort M, score 3. No issue filed yet. Hit by: Linux switcher, Singer-songwriter.

The release notes say a take lands one period late through a PipeWire loopback; there is no calibration or manual offset, and the latency readout shows one buffer, not the round trip.

### T47 Mixer on touch and small screens

Impact 3, effort M, score 1.5. No issue filed yet. Hit by: Steam Deck / touch.

Pan and send sliders fight the horizontal flick; no numeric entry for fader, pan or sends; plugin windows are unscaled on a 1280x800 screen (related #162).

## What already works well

Things the walkthroughs singled out as good, worth keeping as they are:

**Bedroom producer**

- First-run empty state is clear and native: Kirigami.PlaceholderMessage with a helpful action and recent projects (ArrangePage.qml:568-609); audio failures surface a notification with an 'Audio Settings…' button (Main.qml:582-592).
- Drag-and-drop of audio from Dolphin onto the timeline, multi-file import with automatic new tracks, and wide format reading (WAV/FLAC/AIFF/MP3/Ogg) (ArrangePage.qml:100-123, 553-566).
- Project safety is well beyond a typical 0.1: project-as-folder, ten rolling backups, autosave recovery dialog, Collect Audio Files and Find Missing Audio Files (Main.qml:176-195, 325-369; user-guide.md:52-59).
- Everything is undoable with named undo/redo entries ('Undo Move Clip') (Main.qml:197-214), including recorded takes and instrument swaps.

**Singer-songwriter**

- Take alignment is done properly: recorded audio is placed with input + output + graph (plugin delay) latency compensation, re-read at stop when port latencies have settled (crates/daw-session/src/record.rs:1-25, crates/daw-app/src/recording.rs:357-359). Overdubs line up without the user knowing what latency is.
- Recording is robust and honest: 32-bit float WAV so a quiet take can be raised safely (record.rs:167), per-track problems are reported while the rest keep recording, and xruns, disk stalls and dropped samples each produce a plain-language inline message (ProjectMessageText.qml:140-157).
- Monitoring defaults are thoughtful: Off by default so a laptop mic does not howl, and the Auto mode is tape-style (hear input when stopped or recording, hear the take on playback) and passes through the track's inserts and sends, so reverb in the headphones works with no extra routing (model.rs:547-555, session lib.rs:856-867).
- Count-in is easy to find and clicks even when the metronome is off; the position display says 'Count-in' while it runs; metronome level and count-in are reachable from both the transport bar and a dedicated mixer strip (TransportBar.qml:206-268, 70-72).

**Electronic / hip-hop beatmaker**

- Piano-roll note preview through the track's own instrument, including glissando along the keys and re-audition while dragging to a new pitch (crates/daw-pianoroll/src/lib.rs:391-411; EditPage.qml:206).
- Loop recording overdubs every pass into one take, held notes carry across the loop seam, and pressing R with nothing armed records into the open clip (crates/daw-midi/src/record.rs:11-13; crates/daw-app/src/recording.rs:1-14).
- A solid set of note operations with single-key shortcuts: Quantize (Q, Shift+Q, 50%), Legato, Glue, Fix Overlaps, Reverse, Humanize, Split at Playhead, Duplicate, arrow-key transpose and nudge (EditPage.qml:50-170).
- Velocity lane that edits chord stems together and limits drawing to the selection when there is one (crates/daw-pianoroll/src/lib.rs:499-547).

**Podcaster / voice-over editor**

- Recording is solid for voice: per-track mono or stereo input choice with 'Monitor Input While Armed' (TrackHeader.qml ~l.267-326), 32-bit float takes (daw-session/src/record.rs:167), latency-compensated placement, takes longer than one WAV roll over into further clips, and xruns/dropped input are reported (crates/daw-app/src/recording.rs:248-266).
- Long files are handled well under the hood: audio streams from disk (daw-media DiskStreamer) and waveform peaks are built on a background thread and cached next to the audio (crates/daw-arrange/src/peaks.rs:1-45), so reopening an hour-long project is quick.
- Non-destructive clip handles are discoverable and pleasant: fade in/out corners with three shapes, a clip gain handle with live dB tooltip and Shift for fine control, and the waveform redraws at the new gain (lib.rs:57-62, draw.rs:398-402).
- Project safety is better than most 0.1 releases: self-contained project folders, ten rolling backups per save, autosave recovery copies with a recovery dialog, relink of missing audio and Collect Audio Files (user-guide.md:46-59; Main.qml:171-196).

**Film / game composer**

- Plugin delay compensation is real: the graph compiler computes per-path latency and inserts compensating delays on every edge (daw-engine compile.rs:308-349), and recording accounts for graph latency (daw-session record.rs:128). Composers with lookahead limiters and convolution reverbs need this and get it without any setup.
- Each plugin runs in its own sandbox process with crash detection and Reload Plugin (daw-plugin-host lib.rs:1-20; DevicePanelDrawer.qml:266). For big templates full of third-party sample players, one bad plugin not taking down the session is a major selling point.
- The engine and file format already carry a full tempo and meter map with ramps (model.rs:319-459; SetTempo commands.rs:150), export honours tempo changes, and the grid/bar numbering follow meter changes. The hardest part of scoring support is built; it only lacks UI.
- Bus architecture fits orchestral mixing: buses, bus-to-bus output routing, pre/post-fader sends, resizable strips, per-track output selection (ChannelStrip.qml:191-268, 407).

**Experienced Linux musician switching from Ardour / Bitwig / Reaper / Qtractor**

- Every plugin runs in its own process: a crash bypasses the device, shows an inline 'Reload Plugin' action in the device panel and the session keeps running (DevicePanelDrawer.qml:239-271); missing plugins keep their state and the project still opens.
- CLAP, VST3 and LV2 are all first-class in one searchable browser, scanned out of process in the background with progress, per-bundle failure reasons, extra folders, and $CLAP_PATH/$VST3_PATH/$LV2_PATH honoured (so yabridge folders and custom layouts work).
- PipeWire/JACK-native behaviour a Linux user expects: each MIDI device gets its own named input port with hot-plug reconnect and per-track device/channel selection (host.rs:217-330, MidiInputButton.qml), and programs' MIDI outputs are listed as sources too.
- Recording is latency-compensated using the measured round-trip latency (recording.rs:191, 358), with input monitoring per track, count-in, and xruns during a take reported afterwards.

**Steam Deck / touch / small-screen user**

- Long-press context menus already exist on track headers, automation lane headers, clips, empty lanes and automation points on the Arrange page (TrackHeader.qml:416-423, LaneHeader.qml long-press TapHandler, ArrangePage.qml:426-434, 519-524), and the menus are built lazily so scrolling stays smooth.
- Arrange page touch basics are in place: one-finger pan on empty lanes and on the header column, two-finger pinch zoom of time, and a deliberate hand-over so a second finger can start a pinch (ArrangePage.qml:324-336, 416-476, 542-550).
- Every clip and note command is also a Kirigami.Action in the page toolbar's overflow menu (Split, Duplicate, Delete, Select All, Zoom, Quantize, Transpose, Legato...), so nothing depends solely on right-click (ArrangePage.qml:127-205, EditPage.qml:50-261).
- Track reordering has a no-drag path: Move Up / Move Down in the track menu (TrackHeader.qml:378-389).

## The personas and their stories

Outcome of each story against Kadence 0.1.0: *works*, *works awkwardly*,
*partly missing*, *missing* or *breaks*.

### Bedroom producer (solo pop/indie, in-the-box)

A self-taught solo producer making complete pop/indie songs on a Plasma desktop: programs drums and bass with instrument plugins and one-shot samples, drags in a few loops, records a handful of vocal/guitar parts, arranges verse/chorus sections, mixes, and bounces a master to send to friends or upload. They learned from GarageBand, FL Studio and Ableton Live Lite tutorials, so they expect Ctrl+C/Ctrl+V everywhere, a drum sampler you can drop samples into, loops that follow the project tempo, coloured tracks, markers, presets, and an MP3 export. Success is finishing a 3-minute song in one tool without touching a terminal, Audacity or ffmpeg, and without rebuilding the same setup for every new song.

| Story | Outcome | As this user, I want… |
| --- | --- | --- |
| BP-1 | works awkwardly | As a bedroom producer, I want to launch Kadence and start a new song from a sensible starting point (my usual drum/bass/vocal tracks and reverb bus) so that I can get to making music instead of rebuilding my setup every time. |
| BP-2 | breaks | As a bedroom producer, I want to load kick, snare and hat samples into a drum instrument and program a beat so that my song has drums made from my own sample pack. |
| BP-3 | works awkwardly | As a bedroom producer, I want to write a bass line and chords with a synth, picking a sound from presets and fine-tuning notes on a grid I choose, so that parts come together quickly even though I'm not a keyboard player. |
| BP-4 | partly missing | As a bedroom producer, I want to drag a drum loop or sample from my sample pack into the song and have it match my project tempo and repeat for a section so that I can build on loops quickly. |
| BP-5 | works awkwardly | As a bedroom producer, I want to record a vocal and a guitar part over my beat, redoing the chorus a few times, so that I get a good take without leaving the flow. |
| BP-6 | partly missing | As a bedroom producer, I want to lay out intro / verse / chorus / bridge by copying sections around, label them, and loop one section while I work on it, so that my 8-bar idea becomes a full song. |
| BP-7 | partly missing | As a bedroom producer, I want to colour-code and group my tracks (drums blue, vocals orange) and name my clips so that a 20-track song stays readable. |
| BP-8 | works | As a bedroom producer, I want to balance levels, add EQ/compression/reverb, automate a filter sweep, and make the master loud without clipping so that the song sounds finished. |
| BP-9 | missing | As a bedroom producer on a modest laptop, I want to freeze or bounce CPU-heavy instrument tracks to audio, and export stems for a friend who mixes, so that the project keeps playing without crackles and I can collaborate. |
| BP-10 | partly missing | As a bedroom producer, I want to export the finished song as an MP3 (and a WAV) at a sensible loudness with the title and my artist name so that I can send it to friends and upload it. |

### Singer-songwriter (acoustic guitar + vocals, 2-input USB interface)

Records acoustic guitar and vocals at home through a 2-in/2-out USB interface on a Plasma desktop, usually to a click, one or two tracks at a time. Comes from Audacity, GarageBand, Reaper or Ardour, so expects an input meter, takes/comping, punch-in and a quick "try that again" loop without thinking about it. Does not care about MIDI or synths; cares about getting a clean take down fast while the performance is fresh, then picking the best phrases, adding EQ/compression/reverb, and exporting something to upload or send to a friend. Success is: plug in, see level, record four takes of a verse, comp them, light mix, and export a WAV/MP3 in one evening without losing a take or hunting for files.

| Story | Outcome | As this user, I want… |
| --- | --- | --- |
| SS-1 | works awkwardly | As a singer-songwriter, I want to launch Kadence, pick my interface input and see that my mic/guitar level is healthy before I record, so that I do not ruin a take with clipping or a dead input. |
| SS-2 | works awkwardly | As a singer-songwriter, I want to hear myself in the headphones with a little reverb while I track vocals, so that I pitch better and enjoy the performance. |
| SS-3 | works | As a singer-songwriter, I want to set a tempo, turn on a click with a count-in and record a guitar take, so that later overdubs line up. |
| SS-4 | works awkwardly | As a singer-songwriter, I want to throw away a bad take and immediately go again from the same spot, so that I keep my momentum. |
| SS-5 | missing | As a singer-songwriter, I want to loop the chorus and sing it four or five times in a row, keeping every pass as a take, so that I can choose the best one afterwards. |
| SS-6 | missing | As a singer-songwriter, I want to punch in over one bad line in the second verse of an otherwise good vocal take, so that I fix it without re-singing the whole song. |
| SS-7 | partly missing | As a singer-songwriter, I want to cut my takes into phrases, keep the best of each, slide them into place and smooth the joins, so that I end up with one composite vocal. |
| SS-8 | partly missing | As a singer-songwriter, I want to tune my guitar before recording and even out the level of quiet and loud takes afterwards, so that the tracks are in tune and sit at a consistent level before I add a compressor. |
| SS-9 | works | As a singer-songwriter, I want to put an EQ and compressor on the vocal, send vocal and guitar to a shared reverb, and balance the two, so that the song sounds finished enough to share. |
| SS-10 | works awkwardly | As a singer-songwriter, I want to export the finished song to a file I can upload or message to a friend, at a sensible loudness and without clipping, so that the deliverable just works. |
| SS-11 | works awkwardly | As a singer-songwriter, I want my recordings to live inside the project folder and be safe when I copy the project to a backup drive, so that I never lose a take. |

### Electronic / hip-hop beatmaker

Builds beats from one-shot samples and loops, lives in the piano roll and a drum grid, plays pads and a small MIDI keyboard, and leans on sidechain compression, automation and many plugin instruments and effects. Comes from FL Studio, Ableton Live or Bitwig, so expects a sample browser with audition, a drum sampler, copy/paste everywhere, a selectable grid with swing, and warping of loops to project tempo. Success is going from an empty project to a looped 8-bar beat in minutes, arranging it into a song, and exporting a master plus stems. All file references below are relative to /home/user/kadence.

| Story | Outcome | As this user, I want… |
| --- | --- | --- |
| BM-1 | works | As a beatmaker, I want to launch Kadence, get sound out, and have my pad controller and plugins recognised so that I can start a beat immediately. |
| BM-2 | partly missing | As a beatmaker, I want to browse my sample folders, audition one-shots and loops in time with the project, and drag the ones I like into the beat so that I can pick sounds quickly. |
| BM-3 | breaks | As a beatmaker, I want to load kick, snare and hat one-shots into a drum sampler and play them from my pads so that I can program a kit on one track. |
| BM-4 | works awkwardly | As a beatmaker, I want to click in a drum pattern on a 1/16 grid with some triplet hat rolls and swing so that I can program a groove quickly. |
| BM-5 | works awkwardly | As a beatmaker, I want to write an 808 line and a melody in key, copying phrases between bars and clips while seeing the chords from another track, so that parts fit together. |
| BM-6 | works awkwardly | As a beatmaker, I want to loop four bars and finger-drum layers from my pad controller, with pitch bend and mod wheel moves captured and the take quantized, so that I can build a groove by playing. |
| BM-7 | missing | As a beatmaker, I want to drop a 92 BPM drum break into my 140 BPM project, have it follow the tempo, chop it into slices and rearrange or repeat them so that I can flip a sample. |
| BM-8 | works awkwardly | As a beatmaker, I want to turn my 8-bar loop into a full arrangement (intro, verse, hook, drops with muted parts) so that I have a song structure. |
| BM-9 | missing | As a beatmaker, I want the kick to duck the 808 and pads with sidechain compression so that the low end stays clean and the track pumps. |
| BM-10 | works | As a beatmaker, I want to automate a filter sweep into the drop and add rhythmic movement to a pad so that the beat evolves. |
| BM-11 | works awkwardly | As a beatmaker with a few hundred plugins, I want to find an instrument or effect fast, try presets, and reuse my favourite chains so that sound design does not interrupt the flow. |
| BM-12 | partly missing | As a beatmaker, I want to export the finished beat as a master WAV, an MP3 to send to artists, and stems of each track for mixing so that I can deliver it. |

### Podcaster / voice-over editor

Records one or two microphones, or imports hour-plus recordings (often from Zoom, a phone or a field recorder), then cuts mistakes and silences, closes the gaps, adds intro/outro music with ducking, levels loudness to a podcast target (-16 LUFS stereo / -19 mono) and delivers a tagged MP3. Comes from Audacity, Reaper or Hindenburg, so thinks in minutes:seconds, time-range selections and ripple edits, never in bars, beats or tempo. Success is: a one-hour episode imported, edited with keyboard-driven cut-and-close, loudness-compliant and exported as MP3 with title/artist/chapters, without ever touching the tempo field. Kadence 0.1 is explicitly framed as 'making a song' (docs/user-guide.md:3-5), so this persona is off the paved road almost everywhere outside recording and basic clip handling.

| Story | Outcome | As this user, I want… |
| --- | --- | --- |
| POD-1 | partly missing | As a podcaster, I want to set up a spoken-word session on first launch (no click, no count-in, a timeline in minutes:seconds) so that the app matches how I think about an episode. |
| POD-2 | works awkwardly | As a podcaster, I want to record my USB mic (and a co-host's) as mono tracks while watching input levels and elapsed time so that I know the take is healthy while I talk for an hour. |
| POD-3 | works awkwardly | As a podcaster, I want to import a 70-minute interview (WAV from a recorder, or M4A/Opus from Zoom or a phone) so that I can start editing quickly and see the whole waveform. |
| POD-4 | works awkwardly | As a podcaster, I want to move through the episode by keyboard (click in the waveform to place the cursor, jump back a few seconds, play faster, go to the end) so that I can find mistakes quickly. |
| POD-5 | missing | As a podcaster, I want to select a stretch of time across both mic tracks, delete it and have everything after it close up (ripple) so that removing a flubbed answer takes one gesture. |
| POD-6 | missing | As a podcaster, I want to automatically remove or shorten long silences and reduce background noise so that I do not hand-edit every pause in an hour of audio. |
| POD-7 | partly missing | As a podcaster, I want to even out levels between two speakers (clip gain, normalize, compression) so that listeners do not ride their volume knob. |
| POD-8 | missing | As a podcaster, I want to meter and hit a loudness target (-16 LUFS integrated, -1 dBTP) on the master so that my episode meets platform specs. |
| POD-9 | works awkwardly | As a podcaster, I want to add intro and outro music that fades in and automatically ducks under the voice so that the opening sounds produced without hand-drawing every dip. |
| POD-10 | missing | As a podcaster, I want to drop markers for mistakes while recording and name chapter points while editing so that I can find edit points and publish chapters. |
| POD-11 | partly missing | As a podcaster, I want to export the finished episode as a mono or stereo MP3 (or Opus/M4A) with title, artist, episode number and cover art, over an exact time range, so that I can upload it straight to my host. |
| POD-12 | works awkwardly | As a podcaster producing a weekly show, I want to start each episode from my saved setup (tracks, voice chain, intro music) and trust that a two-hour edit will not be lost so that production is repeatable and safe. |

### Film / game composer

Writes orchestral and hybrid scores to picture, working from a large template (dozens to hundreds of instrument tracks hosting sample-library plugins) and delivering a stereo mix plus stems at 48 kHz/24-bit. Comes from Cubase/Nuendo, Logic Pro, Digital Performer or REAPER, where a tempo track, markers/hit points, video playback with timecode, MIDI CC lanes, expression/articulation maps, folder tracks, track visibility and batch stem export are assumed. Success means: open a template, load a cue's picture and guide audio, build a tempo/meter map that lands on hit points, perform and edit parts with CC1/CC11 dynamics, keep a 100+ track session navigable and playable, then deliver named stems that line up with the picture. In Kadence 0.1 the mixer side (buses, sends, latency compensation, sandboxed plugins) is closer to this persona's needs than the arranging side, which is song-oriented (single tempo, no markers, no picture, notes-only MIDI).

| Story | Outcome | As this user, I want… |
| --- | --- | --- |
| FC-1 | works awkwardly | As a film composer, I want to start each cue from my orchestral template (tracks, instruments, buses, routing already set up) so that I do not rebuild a 100-track session for every cue. |
| FC-2 | partly missing | As a film composer, I want to organise 150 tracks into section folders (Strings, Brass, Perc, Synths), colour them, hide what I am not using and jump to a track by name so that a huge template stays navigable. |
| FC-3 | missing | As a film composer, I want to load the cue's video and guide audio, see timecode, and set where the cue starts in the reel so that everything I write is in sync with picture. |
| FC-4 | missing | As a film composer, I want to drop named markers at hit points (cuts, door slam, title card) and jump between them so that I can design the tempo map around them and navigate a long cue. |
| FC-5 | partly missing | As a film composer, I want to add tempo changes, ramps and time-signature changes at specific bars so that downbeats land on my hit points. |
| FC-6 | missing | As a film composer, I want to record and edit mod wheel (CC1), expression (CC11) and sustain pedal along with my notes so that sampled strings and brass have shaped dynamics. |
| FC-7 | missing | As a film composer, I want one multi-timbral sampler instance to serve several MIDI tracks on different channels with separate audio outputs, and to switch articulations by name, so that my template stays light and readable. |
| FC-8 | partly missing | As a film composer, I want to copy a motif to other tracks and bars, and insert or remove bars when the picture is recut, so that I can orchestrate quickly and conform a cue to a new edit. |
| FC-9 | partly missing | As a film composer, I want a 120-track template full of sample-library plugins to play back without dropouts, and to freeze or disable heavy tracks, so that I can keep writing as the cue grows. |
| FC-10 | works | As a film composer, I want to mix sections through buses with shared reverbs and automate dynamics so that the mock-up sounds finished, with everything staying time-aligned. |
| FC-11 | partly missing | As a film composer, I want to export the full mix plus stems (strings, brass, percussion, synths, FX) as 48 kHz/24-bit WAVs of identical length in one pass so that I can deliver to the dub stage. |

### Experienced Linux musician switching from Ardour / Bitwig / Reaper / Qtractor

A long-time Linux audio user who runs PipeWire day to day and real JACK for serious sessions, with a multichannel interface, outboard synths/effects and a large LV2/CLAP/VST3 library (including yabridge-bridged Windows plugins). They come from Ardour, Reaper, Bitwig or Qtractor, so they expect patchbay-friendly port handling, JACK transport/MIDI clock, per-track hardware routing, customisable shortcuts, MIDI file and stem interchange, and power editing (copy/paste, ripple, snap modes, markers, punch/takes). They care more about robustness and interoperability than about polish: a session must survive plugin crashes, be scriptable/launchable from a session manager, and export deliverables other people can use. Success is recording a band or hybrid hardware/software track, mixing it with their usual plugins, and handing over stems or a master without having to fall back to their old DAW.

| Story | Outcome | As this user, I want… |
| --- | --- | --- |
| LM-1 | partly missing | As a Linux musician with an 8-in/8-out interface on PipeWire/JACK, I want to configure the audio backend and route tracks to specific hardware inputs and outputs so that I can record several sources and feed a headphone mix or outboard gear. |
| LM-2 | missing | As a hybrid hardware/software musician, I want Kadence to sync with other JACK clients and drive my external synths and outboard effects so that it fits into my existing studio instead of replacing it. |
| LM-3 | works awkwardly | As a plugin-heavy user with hundreds of LV2/CLAP/VST3 plugins (some via yabridge), I want to scan, organise, find and recall plugins and their presets quickly so that mixing is not slowed down by plugin housekeeping. |
| LM-4 | missing | As a keyboard-driven Reaper/Ardour user, I want to remap shortcuts and drive transport, navigation and track state from the keyboard so that my muscle memory carries over. |
| LM-5 | missing | As someone moving a half-finished song over from Ardour/Qtractor, I want to import its MIDI files and audio stems (and later export MIDI back) so that I do not have to re-record or re-enter parts. |
| LM-6 | works awkwardly | As a player recording guitar and keys, I want to do several takes with punch-in over a loop and keep my pedal and pitch-bend data so that I can capture a good performance without stopping. |
| LM-7 | partly missing | As an arranger used to Ardour/Reaper editing, I want to copy and paste clips and notes, control snapping, drop markers and restructure the song quickly so that arranging does not become mouse-only busywork. |
| LM-8 | works awkwardly | As a mixer, I want to build a mix with buses, sends, my usual plugins, sidechain compression and automation so that the result matches what I can do in my previous DAW. |
| LM-9 | works awkwardly | As someone delivering to a mastering engineer and a collaborator, I want to export the master plus stems in the formats they ask for so that I can hand over the project in one go. |
| LM-10 | partly missing | As a user who launches sessions from Dolphin, a terminal or a session manager (RaySession/Agordejo), I want Kadence to open a given project, cooperate with NSM, and keep its files where I expect so that it slots into my studio setup and backups. |
| LM-11 | works | As a Plasma Wayland user with a dark custom colour scheme and fractional scaling, I want Kadence and plugin windows to look and behave natively, and to stay stable when a plugin misbehaves, so that I can trust it for real sessions. |

### Steam Deck / touch / small-screen user

Runs Kadence in Plasma desktop mode on a 1280x800 Steam Deck (or a convertible laptop / tablet), driving it with the touchscreen, the trackpads-as-mouse and gamepad buttons mapped by Steam Input, usually with no physical keyboard, no scroll-wheel modifiers and no MIDI hardware; sometimes docked to a monitor. They come from touch-first tools (GarageBand for iPad, FL Studio Mobile, Cubasis, Koala, BandLab) and from desktop DAWs with decent touch support (Bitwig, Reaper, Ardour). Their goal is to sketch a song on the sofa: tap in a beat or melody, record a voice idea through the built-in mic, arrange, do a rough mix and export a file. Success means every step has a path that needs no right-click, hover, keyboard modifier or scroll wheel, with targets a finger can hit and views that pan and zoom like every other touch app.

| Story | Outcome | As this user, I want… |
| --- | --- | --- |
| SD-1 | works awkwardly | As a Steam Deck user, I want to launch Kadence with no audio interface attached and understand the screen at 1280x800 so that I can start a song without a mouse or manual. |
| SD-2 | missing | As a Steam Deck user with no MIDI keyboard, I want to play and record notes from an on-screen piano or pads so that I can get a melody or beat into an instrument track. |
| SD-3 | works awkwardly | As a Steam Deck user, I want to record a vocal idea through the built-in microphone over a click so that I can capture it without an audio interface. |
| SD-4 | works awkwardly | As a touchscreen user, I want to pan and zoom the arrangement with my fingers so that I can find my way around a song as in any other touch app. |
| SD-5 | partly missing | As a touchscreen user, I want to move, trim, fade, split and select several clips with my finger so that I can arrange a song without a mouse. |
| SD-6 | partly missing | As a keyboardless user, I want to loop bars 5 to 9 while I work on them so that I can practise and edit a section, and later export just that range. |
| SD-7 | partly missing | As a touchscreen user, I want to enter and edit notes in the piano roll with my finger so that I can write a drum pattern and a bass line. |
| SD-8 | works awkwardly | As a touchscreen user, I want to balance levels, add a reverb send and tweak an effect on the Mix page so that I can get a rough mix on the Deck. |
| SD-9 | works awkwardly | As a touchscreen user, I want to draw a volume fade and record a filter sweep as automation so that my mix moves over time. |
| SD-10 | works awkwardly | As a keyboardless user, I want to name tracks, save the project, pick a plugin and export a file so that I can finish and share the song using only the on-screen keyboard when text is unavoidable. |
| SD-11 | missing | As a Steam Deck user, I want the gamepad buttons to drive the transport and common edits so that I can play, stop, record and undo without touching the screen. |

## Method and limits

- Each persona was researched independently against the same checkout, so the
  same gap was often found several times; the themes above merge those.
- A finding counts as "missing" when there is no path to it in the interface,
  even if the model or engine supports it.
- Outright bugs were left to the code review running at the same time (issues
  labelled `adversarial-review`), except where a story ran straight into one.
- Effort is an estimate from reading the code, not from a design.
- Nothing here was run on a device, and no real users were interviewed. The
  obvious next step for any theme before building it is to try the current
  flow in the running application.
