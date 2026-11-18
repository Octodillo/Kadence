# Kadence user guide

Kadence is a digital audio workstation for KDE Plasma. This guide walks
through making a song: setting up audio, recording, arranging, editing
MIDI, mixing and exporting. It describes Kadence 0.1.

## Getting started

### Audio

Kadence plays and records through PipeWire's JACK interface, which every
Plasma desktop has, or through JACK itself. Open **Configure Kadence…**
(Ctrl+Shift+,) and pick **Audio**:

- **Device**: the output Kadence plays through. *JACK / PipeWire output* is
  the usual choice.
- **Sample rate** and **Buffer size**: a smaller buffer lowers the latency
  (shown next to it) but needs a faster computer. 128 or 256 frames is a
  good start; lower it if you play instruments live.

If there is no audio server, Kadence still runs and shows a notice; it
plays silently until you choose a working device.

### MIDI

Plug in a MIDI keyboard or controller before or after starting Kadence. The
**MIDI** settings page lists the devices PipeWire sees and which of them
new tracks listen to. Each track can also listen to just one device or
channel (see [Recording](#recording)).

### Plugins

Kadence looks for CLAP, VST3 and LV2 plugins in these folders and the
folders inside them, in this order:

| Format | Folders |
|---|---|
| CLAP | the folders in `$CLAP_PATH`, `~/.clap`, then `clap` in each system folder |
| VST3 | the folders in `$VST3_PATH`, `~/.vst3`, then `vst3` in each system folder |
| LV2 | `~/.lv2`, then `lv2` in each system folder |

The system folders are `/usr/local/lib`, `/usr/local/lib64`, `/usr/lib`,
`/usr/lib64` and, on Debian and Ubuntu, `/usr/lib/x86_64-linux-gnu` (or the
one named after your computer's architecture). So CLAP plugins from your
distribution are found in `/usr/lib/clap` or `/usr/lib64/clap`, for example.

`CLAP_PATH`, `VST3_PATH` and `LV2_PATH` are optional environment variables
that each hold a list of folders separated by colons. The folders in
`CLAP_PATH` and `VST3_PATH` are searched before the others; `LV2_PATH`, if
set, replaces the LV2 folders above.

The plugin section of the **Audio** settings page lists the ones of these
folders that exist on your system, along with those in your home folder,
and shows what was found. **Add Folder…** adds
another folder, which is searched after these for all three formats, and
**Rescan** looks again. Each plugin
runs in a process of its own, so a plugin that crashes is switched off and
reported instead of closing Kadence.

That process asks for real-time priority, directly or through RealtimeKit
(`rtkit`) or the desktop portal, so the plugin's audio keeps up while the
computer is busy. A plugin that did not get it still plays, but its sound
may drop out under load, and its device panel says so. If no plugin gets
it, Kadence also says so once. Most Plasma desktops run RealtimeKit; check
that the `rtkit` package is installed and its service running.
RealtimeKit grants a limited number of programs, so with many plugins some
may go without: allowing your user real-time priority (on many
distributions, by joining the `realtime` or `audio` group) lifts the
limit.

In the Flatpak, install plugins from Flathub (their IDs start with
`org.freedesktop.LinuxAudio.Plugins.`) or put them in `~/.clap`, `~/.vst3`
or `~/.lv2`, which the Flatpak may read; if you had to create the folder,
restart Kadence so it can see it. The Flathub plugins are found in
`/app/extensions/Plugins/clap`, `/app/extensions/Plugins/vst3` and
`/app/extensions/Plugins/lv2`. The system folders above are then the
Flatpak runtime's, not your system's, so plugins installed with your
distribution's packages are not found.

## Projects

A project is a folder that holds the project file (`Song.kadence`) and the
audio it uses, so it can be copied or backed up as one. **New Project**
(Ctrl+N), **Open Project…** (Ctrl+O), **Open Project Folder…**, **Save**
(Ctrl+S) and **Save As…** (Ctrl+Shift+S) are in the main menu, with
**Open Recent**.

- Saving keeps the ten versions before it in the project's `backups`
  folder. Saving again without a change adds nothing to it.
- **Save As…** makes a new project folder and copies the project's audio
  into it. Choosing the open project's own file there just saves it.
- Don't move or rename a project's folder while the project is open:
  Kadence then can't save into it, and says so. Move it back, save, and
  close the project first.
- While a project has unsaved changes, Kadence keeps a recovery copy (you
  can turn this off in **General** settings). After a crash, Kadence offers
  to **Recover** it the next time you open the project.
- Audio recorded before a project's first save is kept with its recovery
  copy and moves into the project folder when you save. Discarding the
  recovery copy deletes those recordings with it. If Kadence crashes when
  there is no recovery copy, the next start moves the recordings to a
  folder of their own in `recordings` in Kadence's data folder (usually
  `~/.local/share/kadence`).
- If audio files were moved, **Find Missing Audio Files…** relinks them.
  **Collect Audio Files** copies audio from outside the project folder into
  it. Only files that open as audio are copied.

Every edit can be undone with Ctrl+Z and redone with Ctrl+Shift+Z.

## The three pages

Kadence has three pages, switched from the sidebar on the left:

- **Arrange** (Ctrl+1): tracks and clips on a timeline.
- **Mix** (Ctrl+2): every channel strip side by side, with its meter. You
  never need it: the Arrange page has all of each track's controls (see
  [Controls and automation in the Arrange page](#controls-and-automation-in-the-arrange-page)).
  It is there for an overview of the whole mix.
- **Edit** (Ctrl+3): the piano roll for the open MIDI clip.

**Play**/**Stop** (Space) and **Record** (R) are on the toolbar of the
Arrange and Edit pages, and **Loop** (Ctrl+L) on the Arrange page's.
**Go to Start** (Home) is in the sidebar, with **Stop All Sound**
(Shift+Space): **Stop** lets reverb and delay tails ring out, and **Stop All
Sound** cuts them off. Under the Arrange page's toolbar,
the transport bar shows the position in bars and beats, the tempo and time
signature, and the metronome.

## Tracks

**Add Track** offers three kinds:

- **Audio Track** (Ctrl+T) for recorded or imported audio.
- **Instrument Track** (Ctrl+Shift+T) for MIDI. It starts with Kadence's
  synth; click the instrument button on the track header or the channel
  strip to choose the sampler or a plugin instrument instead.
- **Bus**, to group tracks or share an effect such as reverb.

Right-click a track header to rename, move, delete or show its automation.
On a touch screen, hold a finger on it; from the keyboard, press Menu or
Shift+F10 on a focused header or timeline. The page menu also has **Rename
Track…** (F2), **Move Track Up** and **Down** (Alt+Up, Alt+Down) and **Delete
Track** for the selected track.
**M** mutes a track and **S** solos it.

## Recording

1. **Arm** the track (the record button on its header).
2. Choose what it records: the audio input button on an audio track, or the
   MIDI input button on an instrument track (*all inputs*, one device, or
   one channel of a device). An armed instrument track plays what you play,
   even while stopped.
3. Turn on the **Metronome** if you want a click. Its settings set the
   count-in and the volume.
4. Press **Record** (R). Press it again, or **Stop**, to finish.

MIDI recorded over an open MIDI clip goes into that clip; otherwise a new
clip is made. A recorded take can be undone like any edit.

Closing Kadence, or starting or opening another project, while recording
ends the take and keeps it, so you are asked whether to save it like any
other change. **Save As…** (and the first save of a new project) ends the
take too; **Save** in a saved project lets it run on.

## Arranging

- **Import Audio…** (Ctrl+I) adds audio files at the playhead, or drop
  files from Dolphin onto the timeline. WAV, FLAC, AIFF, MP3 and Ogg Vorbis
  are read.
- Double-click an empty spot on an instrument track to add a one-bar MIDI
  clip.
- Drag a clip to move it, drag its edges to trim it. Edits snap to the grid.
- Right-click a clip for **Split at Playhead** (Ctrl+E), **Duplicate**
  (Ctrl+D), **Delete**, the fade shape, **Remove Fades** and **Reset Clip
  Gain**.
- Drag the small handles at an audio clip's top corners to fade it in and
  out, and the one at the top centre up or down to change its gain.
- Shift-drag in the ruler to set the loop range.
- Zoom with Ctrl+plus and Ctrl+minus, or **Zoom to Fit** (Ctrl+0).

### Tempo and time signature

Scroll or drag the tempo in the transport bar to change it, or **Tap** along
to a beat. The time signature is set next to it.

## Editing MIDI

Double-click a MIDI clip to open it on the **Edit** page.

- Double-click empty space to add a note; double-click a note to delete it.
  Keep the button down after the double-click to hear the new note (with
  **Preview Notes** on), and drag past its end before letting go to set its
  length.
- Click a note to select it (Shift adds to the selection), drag to move,
  drag an edge to resize. Hold Alt to move without snapping.
- Drag in empty space to select several notes; Ctrl+A selects them all.
- Drag in the velocity lane at the bottom to draw velocities.
- **Quantize** (Q) moves note starts to the grid; **Quantize Starts and
  Ends** (Shift+Q) also snaps their ends; **Quantize 50%** moves them half
  way. **Humanize**, **Legato** and **Fix Overlaps** are in the same menu.
- Up and Down transpose by a semitone, Shift+Up and Shift+Down by an
  octave.
- On a touch screen, a finger on the empty grid scrolls and two fingers
  zoom; a finger on a note moves it, and holding still opens the menu.
  From the keyboard, Menu or Shift+F10 opens it.
- **Flats** on the toolbar, also in the General settings, names the black
  keys as flats (B♭) instead of sharps (A♯).

## Mixing

You can mix entirely in the Arrange page, where each track unfolds into its
controls (see below). The **Mix** page shows the same controls as a mixing
desk, for an overview: a channel strip for every track, then the buses and
the master, with level meters.

- The fader sets the volume (double-click for 0 dB) and the knob the pan
  (double-click to centre).
- **Add Effect** adds a built-in effect (EQ, compressor, delay, reverb,
  utility) or a plugin. Click an effect to open its device panel, where you
  set its parameters, bypass it or move it. Double-click a control to reset
  it. For a plugin, **Show Plugin Interface** opens its own window (an X11
  window, through XWayland on a Wayland session), and **Reload Plugin**
  starts it again if it crashed.
- **Add Send** sends the track to a bus. **Pre** sends before the fader
  instead of after it.
- The output button at the bottom chooses where the track goes: the master
  or a bus.
- Drag the right edge of a strip to make the strips wider or narrower, and
  the line under the effects and sends to give them more or less room.
  Double-click either to go back to the default size.
- The metronome has a strip of its own, with its level, on/off and the
  count-in.

### Controls and automation in the Arrange page

Every track carries its mixer controls with it. Click the arrow at the left
of a track header (or right-click it and choose **Show Controls**) to unfold
them under the track:

- **Volume** and **Pan**, each a row with its slider.
- **Instrument** (on instrument tracks), **Effects** and **Sends**. These
  are groups: click one to open it. Effects lists each effect, and an effect
  (or the instrument) opens to its parameters; Sends lists the level of each
  send. A plugin lists the parameters it lets a host automate, once it has
  loaded.

Each control has a row in the timeline beside it. Click there to start
automating the control: the click adds the first point. Drag points to move
them, double-click one to delete it, and right-click a point to choose the
shape of the curve that leaves it. The **−** button on a row removes its
automation.

Closing a group puts away the controls that have no automation and keeps the
automated ones in view. Closing the track's arrow hides all of its rows.

The rows also change what the track has, as its channel strip on the Mix
page does:

- The **+** on **Effects** adds an effect (a built-in one, or a plugin from
  the browser) at the end of the chain; the **+** on **Sends** adds a send
  to a bus.
- An effect's heading has buttons to show its settings and to remove it.
  The **Instrument** heading shows the instrument's settings; pick another
  instrument with the instrument button in the track header.
- Right-click a send's row to remove it or to take it **Pre-Fader**.
- Right-click the track header and choose **Output** to send the track to
  the master or to a bus.

To record moves instead of drawing them, set a row to **Write** and move its
slider while the song plays, or turn on **Write Automation** on the Mix page
and move the controls there.

## Exporting

**Export Song…** (Ctrl+Shift+E) renders the song to a file:

- **Range**: the whole song, the loop, or a range of bars.
- **Format**: WAV (16-bit, 24-bit, 32-bit float) or FLAC (16-bit, 24-bit),
  with dither when reducing to 16 bits.
- **Tail**: extra time after the end so reverb and held notes can ring out.

## Getting help

Report problems and ideas at <https://github.com/Octodillo/Kadence/issues>.
Include the Kadence version (**About Kadence** in the main menu) and how you
installed it.

When something goes wrong without a message on screen, Kadence writes what
it knows to its standard error, which a Plasma session keeps in the journal
(`journalctl --user -e`). Warnings and errors are written by default; start
Kadence with `KADENCE_LOG=debug` for more, or `KADENCE_LOG=off` for none.
Lines start with `kadence:`, or `kadence-plugin-host:` when they come from
the separate process a plugin runs in.
