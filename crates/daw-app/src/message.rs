//! Messages about the project for the user, as data.
//!
//! Errors from the model, the media decoder and the session arrive here as
//! typed values, and [`Message::fields`] flattens one into a context, a
//! reason and its arguments. The sentences are written in QML
//! (`ProjectMessageText.qml`) so KI18n can translate them. Nothing here may
//! mention Qt.

use std::path::{Path, PathBuf};

use daw_arrange::Refusal;
use daw_media::MediaError;
use daw_model::{FormatError, ModelError};
use daw_session::Warning;
use daw_session::record::TakeProblem;

/// What the user was doing when it went wrong, which picks the headline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Open,
    Save,
    Autosave,
    /// Adding an audio file; `Fields::file` is the file.
    Import,
    Edit,
    Playback,
    /// Recording MIDI or audio.
    Record,
    /// Copying audio from outside the bundle into it.
    Collect,
    /// Getting unsaved work back from an autosave.
    Recover,
    /// Exporting the song to an audio file.
    Export,
}

/// Why, which picks the second sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The file dialog or a drop gave a URL that is not a local file.
    NotLocalFile,
    /// Reading or writing failed; `path` and the system's `detail`.
    FileAccess,
    /// The project file is damaged; `detail` says where.
    Malformed,
    /// Not a Kadence project; `detail` is the format it claims.
    NotAProject,
    /// Saved by a newer Kadence; `number` is its schema, `number2` ours.
    TooNew,
    /// An old schema this build cannot upgrade; `number` is the schema.
    NoMigration,
    /// The project contradicts itself; `detail`.
    Inconsistent,
    /// Save needs a location first.
    NoLocation,
    /// Unsaved changes from a crash wait at `path`.
    AutosavePending,
    /// The audio file could not be decoded; `detail`.
    CannotDecode,
    /// The audio file does not say how long it is.
    UnknownLength,
    /// The model refused an edit (a bug); `detail` is English.
    EditRefused,
    /// A note was drawn past the end of its clip.
    NoteOutsideClip,
    /// The audio engine refused the project; `detail`.
    EngineRefused,
    /// Device `name` rejected its saved settings; `detail`.
    DeviceStateRejected,
    /// The project uses built-in device `detail`, which this build lacks.
    UnknownDevice,
    /// Plugin `detail` of device `name` is bypassed until it is loaded.
    PluginNotLoaded,
    /// Plugin `detail` of device `name` is not installed; it is bypassed.
    PluginMissing,
    /// The plugin of device `name` would not load; `detail` is its id and
    /// the plugin's message. It is bypassed.
    PluginLoadFailed,
    /// Plugin `detail` of device `name` crashed; it is bypassed.
    PluginCrashed,
    /// Device `name` did not save its plugin state; `detail`.
    PluginStateNotSaved,
    /// The plugin of device `name` would not show its editor; `detail`.
    PluginEditorFailed,
    /// Audio file `path` cannot be streamed; `detail`.
    CannotStream,
    /// Audio file `path` cannot be read.
    CannotRead,
    /// Notes of track `name` did not reach the engine yet.
    NotesDropped,
    /// Clips of track `name` did not reach the engine yet.
    ClipsDropped,
    /// An output or send of track `name` would loop, so it is left out.
    RoutingLoop,
    /// Recording needs an armed track or an instrument track.
    NoInstrumentTrack,
    /// Recording needs the audio engine running.
    EngineNotRunning,
    /// The track to record on has no MIDI player yet.
    NoMidiPlayer,
    /// MIDI input arrived faster than it could be recorded.
    MidiInputDropped,
    /// The take has no notes inside the song.
    NothingRecorded,
    /// The song was exported to `path`; `number` is its length in seconds.
    Exported,
    /// The user cancelled the export; no file was written.
    ExportCancelled,
    /// There is nothing to export: the song has no clips.
    EmptySong,
    /// "Loop" was chosen but no loop is set.
    NoLoop,
    /// The chosen range is empty or backwards.
    EmptyRange,
    /// Another export is still running.
    ExportBusy,
    /// The audio could not be encoded; `detail`.
    EncodeFailed,
    /// Audio files could not be read fast enough; `number` reads timed out.
    DiskTooSlow,
    /// The file name's extension, `detail`, is another format's.
    WrongExtension,
    /// Sandboxed plugins missed `number` cycles during the export.
    PluginTooSlow,
    /// The plugin of device `name` crashed during the export.
    ExportPluginCrashed,
    /// A plugin changed its latency during the export.
    LatencyChanged,
    /// An audio take could not be written to `path`; `detail`.
    CannotRecordTo,
    /// Track `name` records inputs the audio system doesn't have.
    NoSuchInput,
    /// Track `name` can't record audio yet.
    TrackNotReady,
    /// Writing the take of track `name` stopped early; `detail`.
    RecordingStopped,
    /// `number` frames of track `name`'s take were lost: the disk was slow.
    AudioDropped,
    /// The play position jumped while recording track `name`.
    PositionJumped,
    /// Track `name` was deleted while recording.
    TrackGoneWhileRecording,
    /// The audio system missed `number` cycles while recording.
    RecordXruns,
    /// The bundle at `path` is open in another Kadence.
    AlreadyOpen,
}

/// A message, flattened for a front end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fields {
    pub context: Context,
    pub reason: Reason,
    /// The file being added, for the headline of an import message.
    pub file: Option<PathBuf>,
    /// A path the reason is about.
    pub path: Option<PathBuf>,
    /// A track or device name.
    pub name: String,
    /// Untranslated text from below: a library error, an id.
    pub detail: String,
    pub number: u64,
    pub number2: u64,
}

impl Fields {
    fn new(context: Context, reason: Reason) -> Self {
        Self {
            context,
            reason,
            file: None,
            path: None,
            name: String::new(),
            detail: String::new(),
            number: 0,
            number2: 0,
        }
    }

    fn file(mut self, file: &Path) -> Self {
        self.file = Some(file.to_path_buf());
        self
    }

    fn path(mut self, path: &Path) -> Self {
        self.path = Some(path.to_path_buf());
        self
    }

    fn name(mut self, name: &str) -> Self {
        self.name = name.to_owned();
        self
    }

    fn detail(mut self, detail: impl ToString) -> Self {
        self.detail = detail.to_string();
        self
    }
}

/// What went wrong with recording MIDI.
#[derive(Debug)]
pub enum RecordProblem {
    NoInstrumentTrack,
    EngineNotRunning,
    NoMidiPlayer,
    InputDropped,
    NothingRecorded,
    /// The model refused the take.
    Refused(ModelError),
    /// Something went wrong with an audio take.
    Take(TakeProblem),
    /// The audio system missed this many cycles while recording.
    Xruns(u64),
}

/// What went wrong with exporting the song.
#[derive(Debug)]
pub enum ExportProblem {
    EmptySong,
    NoLoop,
    EmptyRange,
    Busy,
    /// The file name ends in `.wav` or `.flac` but the other format was
    /// chosen; the extension it has.
    WrongExtension(String),
    /// The worker thread could not start.
    Thread(std::io::Error),
    Failed(daw_session::export::ExportError),
}

/// Something to tell the user about the project.
#[derive(Debug)]
pub enum Message {
    NotLocalFile(Context),
    File(Context, FormatError),
    /// Adding `file` failed while decoding it.
    ImportDecode(PathBuf, MediaError),
    /// Adding `file` failed: it has no known length.
    ImportLength(PathBuf),
    /// Adding `file` failed while copying it into the project.
    ImportCopy(PathBuf, FormatError),
    /// Adding `file` failed in the model.
    ImportEdit(PathBuf, ModelError),
    Edit(Refusal),
    Playback(daw_session::SessionError),
    Warning(Warning),
    Record(RecordProblem),
    /// Another Kadence has the bundle at this path open.
    AlreadyOpen(PathBuf),
    Export(ExportProblem),
    /// The song was exported to this file, `seconds` long.
    Exported {
        path: PathBuf,
        seconds: u64,
    },
}

impl From<ModelError> for Message {
    fn from(e: ModelError) -> Self {
        Self::Edit(Refusal::Edit(e))
    }
}

impl From<daw_session::SessionError> for Message {
    fn from(e: daw_session::SessionError) -> Self {
        Self::Playback(e)
    }
}

fn file_fields(context: Context, e: &FormatError) -> Fields {
    use Reason as R;
    match e {
        FormatError::Io { path, source } => Fields::new(context, R::FileAccess)
            .path(path)
            .detail(source),
        FormatError::Json(e) => Fields::new(context, R::Malformed).detail(e),
        FormatError::WrongFormat(format) => Fields::new(context, R::NotAProject).detail(format),
        FormatError::TooNew { found, supported } => Fields {
            number: *found,
            number2: *supported,
            ..Fields::new(context, R::TooNew)
        },
        FormatError::NoMigration(version) => Fields {
            number: *version,
            ..Fields::new(context, R::NoMigration)
        },
        FormatError::Model(e) => Fields::new(context, R::Inconsistent).detail(e),
        FormatError::NoBundle => Fields::new(context, R::NoLocation),
        FormatError::AutosavePending(path) => Fields::new(context, R::AutosavePending).path(path),
    }
}

fn export_fields(p: &ExportProblem) -> Fields {
    use Reason as R;
    use daw_session::export::ExportError as E;
    let new = |reason| Fields::new(Context::Export, reason);
    match p {
        ExportProblem::EmptySong => new(R::EmptySong),
        ExportProblem::NoLoop => new(R::NoLoop),
        ExportProblem::EmptyRange => new(R::EmptyRange),
        ExportProblem::Busy => new(R::ExportBusy),
        ExportProblem::WrongExtension(ext) => new(R::WrongExtension).detail(ext),
        ExportProblem::Thread(e) => new(R::EncodeFailed).detail(e),
        ExportProblem::Failed(e) => match e {
            E::EmptyRange => new(R::EmptyRange),
            E::Cancelled => new(R::ExportCancelled),
            E::Write(MediaError::Io { path, source }) => {
                new(R::FileAccess).path(path).detail(source)
            }
            E::Write(e) => new(R::EncodeFailed).detail(e),
            E::Session(e) => new(R::EngineRefused).detail(e),
            E::DiskTooSlow(n) => Fields {
                number: *n,
                ..new(R::DiskTooSlow)
            },
            E::PluginTooSlow(n) => Fields {
                number: *n,
                ..new(R::PluginTooSlow)
            },
            E::PluginCrashed(device) => new(R::ExportPluginCrashed).name(device),
            E::LatencyChanged => new(R::LatencyChanged),
            // The dialog only offers valid settings.
            E::SampleRate(_) | E::Tail(_) => new(R::EncodeFailed).detail(e),
        },
    }
}

fn take_fields(t: &TakeProblem) -> Fields {
    use Reason as R;
    let new = |r| Fields::new(Context::Record, r);
    match t {
        TakeProblem::CannotCreate { path, detail } => {
            new(R::CannotRecordTo).path(path).detail(detail)
        }
        TakeProblem::NotReady { track } => new(R::TrackNotReady).name(track),
        TakeProblem::NoSuchInput { track } => new(R::NoSuchInput).name(track),
        TakeProblem::WriteFailed { track, error } => {
            new(R::RecordingStopped).name(track).detail(error)
        }
        TakeProblem::Dropped { track, frames } => Fields {
            number: *frames,
            ..new(R::AudioDropped).name(track)
        },
        TakeProblem::Jumped { track } => new(R::PositionJumped).name(track),
        TakeProblem::TrackGone { track } => new(R::TrackGoneWhileRecording).name(track),
    }
}

impl Message {
    pub fn fields(&self) -> Fields {
        use Context as C;
        use Reason as R;
        match self {
            Self::NotLocalFile(context) => Fields::new(*context, R::NotLocalFile),
            Self::File(context, e) => file_fields(*context, e),
            Self::ImportDecode(file, e) => {
                // A decoder error names the path itself; the headline
                // already does.
                let detail = match e {
                    MediaError::Io { source, .. } => source.to_string(),
                    MediaError::Decode(d)
                    | MediaError::Encode(d)
                    | MediaError::Resample(d)
                    | MediaError::Unsupported(d)
                    | MediaError::Peaks(d) => d.clone(),
                };
                Fields::new(C::Import, R::CannotDecode)
                    .file(file)
                    .detail(detail)
            }
            Self::ImportLength(file) => Fields::new(C::Import, R::UnknownLength).file(file),
            Self::ImportCopy(file, e) => file_fields(C::Import, e).file(file),
            Self::ImportEdit(file, e) => {
                Fields::new(C::Import, R::EditRefused).file(file).detail(e)
            }
            Self::Edit(Refusal::Edit(e)) => Fields::new(C::Edit, R::EditRefused).detail(e),
            Self::Edit(Refusal::NoteOutsideClip) => Fields::new(C::Edit, R::NoteOutsideClip),
            Self::Playback(e) => Fields::new(C::Playback, R::EngineRefused).detail(e),
            Self::Record(p) => match p {
                RecordProblem::NoInstrumentTrack => Fields::new(C::Record, R::NoInstrumentTrack),
                RecordProblem::EngineNotRunning => Fields::new(C::Record, R::EngineNotRunning),
                RecordProblem::NoMidiPlayer => Fields::new(C::Record, R::NoMidiPlayer),
                RecordProblem::InputDropped => Fields::new(C::Record, R::MidiInputDropped),
                RecordProblem::NothingRecorded => Fields::new(C::Record, R::NothingRecorded),
                RecordProblem::Refused(e) => Fields::new(C::Record, R::EditRefused).detail(e),
                RecordProblem::Take(t) => take_fields(t),
                RecordProblem::Xruns(n) => Fields {
                    number: *n,
                    ..Fields::new(C::Record, R::RecordXruns)
                },
            },
            Self::AlreadyOpen(path) => Fields::new(C::Open, R::AlreadyOpen).path(path),
            Self::Export(p) => export_fields(p),
            Self::Exported { path, seconds } => Fields {
                number: *seconds,
                ..Fields::new(C::Export, R::Exported).path(path)
            },
            Self::Warning(w) => match w {
                Warning::DeviceStateRejected { device, detail } => {
                    Fields::new(C::Playback, R::DeviceStateRejected)
                        .name(device)
                        .detail(detail)
                }
                Warning::UnknownDevice { kind } => {
                    Fields::new(C::Playback, R::UnknownDevice).detail(kind)
                }
                Warning::PluginNotLoaded { device, plugin } => {
                    Fields::new(C::Playback, R::PluginNotLoaded)
                        .name(device)
                        .detail(plugin)
                }
                Warning::PluginMissing { device, plugin } => {
                    Fields::new(C::Playback, R::PluginMissing)
                        .name(device)
                        .detail(plugin)
                }
                Warning::PluginLoadFailed {
                    device,
                    plugin,
                    detail,
                } => Fields::new(C::Playback, R::PluginLoadFailed)
                    .name(device)
                    .detail(format!("{plugin}: {detail}")),
                Warning::PluginCrashed { device, plugin } => {
                    Fields::new(C::Playback, R::PluginCrashed)
                        .name(device)
                        .detail(plugin)
                }
                Warning::PluginStateNotSaved { device, detail } => {
                    Fields::new(C::Save, R::PluginStateNotSaved)
                        .name(device)
                        .detail(detail)
                }
                Warning::PluginEditorFailed { device, detail } => {
                    Fields::new(C::Playback, R::PluginEditorFailed)
                        .name(device)
                        .detail(detail)
                }
                Warning::CannotStream { path, detail } => Fields::new(C::Playback, R::CannotStream)
                    .path(path)
                    .detail(detail),
                Warning::CannotRead { path } => Fields::new(C::Playback, R::CannotRead).path(path),
                Warning::NotesDropped { track } => {
                    Fields::new(C::Playback, R::NotesDropped).name(track)
                }
                Warning::ClipsDropped { track } => {
                    Fields::new(C::Playback, R::ClipsDropped).name(track)
                }
                Warning::RoutingLoop { track } => {
                    Fields::new(C::Playback, R::RoutingLoop).name(track)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_errors_keep_their_data() {
        let too_new = Message::File(
            Context::Open,
            FormatError::TooNew {
                found: 9,
                supported: 2,
            },
        )
        .fields();
        assert_eq!(
            (too_new.context, too_new.reason),
            (Context::Open, Reason::TooNew)
        );
        assert_eq!((too_new.number, too_new.number2), (9, 2));

        let io = Message::File(
            Context::Save,
            FormatError::Io {
                path: "/x/project.json".into(),
                source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            },
        )
        .fields();
        assert_eq!(io.reason, Reason::FileAccess);
        assert_eq!(io.path.as_deref(), Some(Path::new("/x/project.json")));
        assert!(!io.detail.is_empty());

        let pending = Message::File(Context::Autosave, FormatError::AutosavePending("/b".into()));
        assert_eq!(pending.fields().reason, Reason::AutosavePending);
        assert_eq!(pending.fields().path.as_deref(), Some(Path::new("/b")));
        assert_eq!(
            Message::File(Context::Save, FormatError::NoBundle)
                .fields()
                .reason,
            Reason::NoLocation
        );
    }

    #[test]
    fn import_errors_name_the_imported_file() {
        let file = PathBuf::from("/music/take.flac");
        let copy = Message::ImportCopy(
            file.clone(),
            FormatError::Io {
                path: "/project/audio/take.flac".into(),
                source: std::io::Error::from(std::io::ErrorKind::StorageFull),
            },
        )
        .fields();
        assert_eq!(
            (copy.context, copy.reason),
            (Context::Import, Reason::FileAccess)
        );
        assert_eq!(copy.file, Some(file.clone()));
        // The reason names where the copy failed.
        assert_eq!(
            copy.path.as_deref(),
            Some(Path::new("/project/audio/take.flac"))
        );

        let decode = Message::ImportDecode(file.clone(), MediaError::Decode("bad frame".into()));
        let f = decode.fields();
        assert_eq!(
            (f.reason, f.detail.as_str()),
            (Reason::CannotDecode, "bad frame")
        );
        assert_eq!(f.file, Some(file.clone()));
        assert_eq!(
            Message::ImportLength(file.clone()).fields().reason,
            Reason::UnknownLength
        );
    }

    #[test]
    fn session_warnings_keep_names_and_paths() {
        let f = Message::Warning(Warning::ClipsDropped {
            track: "Drums".into(),
        })
        .fields();
        assert_eq!(
            (f.context, f.reason, f.name.as_str()),
            (Context::Playback, Reason::ClipsDropped, "Drums")
        );
        let f = Message::Warning(Warning::PluginNotLoaded {
            device: "Verb".into(),
            plugin: "clap:org.example.verb".into(),
        })
        .fields();
        assert_eq!(
            (f.reason, f.name.as_str(), f.detail.as_str()),
            (Reason::PluginNotLoaded, "Verb", "clap:org.example.verb")
        );
        let f = Message::Warning(Warning::CannotRead {
            path: "/a.wav".into(),
        })
        .fields();
        assert_eq!(f.path.as_deref(), Some(Path::new("/a.wav")));
    }

    #[test]
    fn recording_problems() {
        let f = Message::Record(RecordProblem::NoInstrumentTrack).fields();
        assert_eq!(
            (f.context, f.reason),
            (Context::Record, Reason::NoInstrumentTrack)
        );
        let f = Message::Record(RecordProblem::Refused(ModelError::Invalid("x".into()))).fields();
        assert_eq!(
            (f.reason, f.detail.is_empty()),
            (Reason::EditRefused, false)
        );
        let f = Message::Record(RecordProblem::Take(TakeProblem::Dropped {
            track: "Vox".into(),
            frames: 480,
        }))
        .fields();
        assert_eq!(
            (f.context, f.reason, f.name.as_str(), f.number),
            (Context::Record, Reason::AudioDropped, "Vox", 480)
        );
        let f = Message::Record(RecordProblem::Take(TakeProblem::CannotCreate {
            path: "/full/take.wav".into(),
            detail: "No space left".into(),
        }))
        .fields();
        assert_eq!(f.reason, Reason::CannotRecordTo);
        assert_eq!(f.path.as_deref(), Some(Path::new("/full/take.wav")));
        let f = Message::Record(RecordProblem::Xruns(3)).fields();
        assert_eq!((f.reason, f.number), (Reason::RecordXruns, 3));
    }

    #[test]
    fn refusals() {
        assert_eq!(
            Message::Edit(Refusal::NoteOutsideClip).fields().reason,
            Reason::NoteOutsideClip
        );
        let f = Message::from(ModelError::Invalid("x".into())).fields();
        assert_eq!((f.context, f.reason), (Context::Edit, Reason::EditRefused));
    }
}
