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

Kadence finds CLAP, VST3 and LV2 plugins in their usual folders
(`~/.clap`, `/usr/lib/clap`, `~/.vst3`, `/usr/lib/vst3`, `~/.lv2`,
`/usr/lib/lv2` and the like). The plugin section of the **Audio** settings
page shows what was found, lets you add other folders with **Add Folder…**,
and **Rescan** looks again. Each plugin
runs in a process of its own, so a plugin that crashes is switched off and
reported instead of closing Kadence.

In the Flatpak, install plugins from Flathub (their IDs start with
`org.freedesktop.LinuxAudio.Plugins.`) or put them in your home folder.

## Projects

A project is a folder that holds the project file (`Song.kadence`) and the
audio it uses, so it can be copied or backed up as one. **New Project**
(Ctrl+N), **Open Project…** (Ctrl+O), **Open Project Folder…**, **Save**
(Ctrl+S) and **Save As…** (Ctrl+Shift+S) are in the main menu, with
**Open Recent**.

- Every save keeps the previous ten versions in the project's `backups`
  folder.
- While a project has unsaved changes, Kadence keeps a recovery copy (you
  can turn this off in **General** settings). After a crash, Kadence offers
  to **Recover** it the next time you open the project.
- If audio files were moved, **Find Missing Audio Files…** relinks them.
  **Collect Audio Files** copies audio from outside the project folder into
  it.

Every edit can be undone with Ctrl+Z and redone with Ctrl+Shift+Z.

## The three pages

Kadence has three pages, switched from the sidebar on the left:

- **Arrange** (Ctrl+1): tracks and clips on a timeline.
- **Mix** (Ctrl+2): a channel strip per track, with effects and sends.
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
- Click a note to select it (Shift adds to the selection), drag to move,
  drag an edge to resize. Hold Alt to move without snapping.
- Drag in empty space to select several notes; Ctrl+A selects them all.
- Drag in the velocity lane at the bottom to draw velocities.
- **Quantize** (Q) moves note starts to the grid; **Quantize Starts and
  Ends** (Shift+Q) also snaps their ends; **Quantize 50%** moves them half
  way. **Humanize**, **Legato** and **Fix Overlaps** are in the same menu.
- Up and Down transpose by a semitone, Shift+Up and Shift+Down by an
  octave.

## Mixing

The **Mix** page has a channel strip for every track, then the buses and the
master.

- The fader sets the volume (double-click for 0 dB) and the knob the pan
  (double-click to centre).
- **Add Effect** adds a built-in effect (EQ, compressor, delay, reverb,
  utility) or a plugin. Click an effect to open its device panel, where you
  set its parameters, bypass it or move it. Double-click a control to reset
  it. For a plugin, **Show Plugin Interface** opens its own window, and
  **Reload Plugin** starts it again if it crashed.
- **Add Send** sends the track to a bus. **Pre** sends before the fader
  instead of after it.
- The output button at the bottom chooses where the track goes: the master
  or a bus.
- Drag the right edge of a strip to make the strips wider or narrower, and
  the line under the effects and sends to give them more or less room.
  Double-click either to go back to the default size.
- The metronome has a strip of its own, with its level, on/off and the
  count-in.

### Automation

Right-click a track header and choose **Show Automation** to see its lanes,
then add a lane for volume, pan or any device parameter. Click in a lane to
add a point, drag points to move them, double-click one to delete it, and
right-click a point to choose the shape of the curve that leaves it. To
record volume, pan and send moves instead, turn on **Write Automation** on
the Mix page and move the controls while the song plays.

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
