// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

import io.github.octodillo.daw

// Words a project message map (Project.status, or what openProject, save
// and saveAs return) in the user's language. Rust sends only data; see
// crates/daw-app/src/message.rs for what each reason fills in.
QtObject {
    // Why it happened, as a full sentence.
    function reason(m: var): string {
        switch (m.reason) {
        case Project.NotLocalFile:
            return i18nc("@info", "Only files on this computer can be used.");
        case Project.FileAccess:
            return i18nc("@info %1 is a file path, %2 a system error message which may be in English",
                         "Could not access “%1”: %2", m.path, m.detail);
        case Project.Malformed:
            return i18nc("@info %1 is a parser message which may be in English",
                         "The project file is damaged (%1).", m.detail);
        case Project.NotAProject:
            return i18nc("@info", "This is not a Kadence project.");
        case Project.TooNew:
            return i18nc("@info %1 and %2 are format version numbers",
                         "It was saved by a newer version of Kadence (format %1; this version reads up to %2).",
                         m.number, m.number2);
        case Project.NoMigration:
            return i18nc("@info %1 is a format version number",
                         "Its format (version %1) is too old for this version of Kadence.", m.number);
        case Project.Inconsistent:
            return i18nc("@info %1 is a technical message which may be in English",
                         "The project contradicts itself (%1).", m.detail);
        case Project.NoLocation:
            return i18nc("@info", "Choose where to save it first.");
        case Project.AutosavePending:
            return i18nc("@info %1 is a file path",
                         "Unsaved changes from an earlier session are saved in “%1”.", m.path);
        case Project.CannotDecode:
            return i18nc("@info %1 is a decoder message which may be in English",
                         "It could not be read as audio (%1).", m.detail);
        case Project.UnknownLength:
            return i18nc("@info", "The file does not say how long it is.");
        case Project.EditRefused:
            return i18nc("@info %1 is a technical message which may be in English",
                         "This is a bug in Kadence; please report it (%1).", m.detail);
        case Project.NoteOutsideClip:
            return i18nc("@info", "Notes must start inside the clip.");
        case Project.EngineRefused:
            return i18nc("@info %1 is a message from the audio engine which may be in English",
                         "The audio engine reported: %1", m.detail);
        case Project.DeviceStateRejected:
            return i18nc("@info %1 is a device name, %2 a message which may be in English",
                         "“%1” could not restore its settings and uses its defaults (%2).", m.name, m.detail);
        case Project.UnknownDevice:
            return i18nc("@info %1 is a device id",
                         "This version of Kadence has no built-in device “%1”; it is bypassed.", m.detail);
        case Project.PluginNotLoaded:
            return i18nc("@info %1 is a device name, %2 a plugin id",
                         "“%1” is bypassed until its plugin (%2) is loaded.", m.name, m.detail);
        case Project.PluginMissing:
            return i18nc("@info %1 is a device name, %2 a plugin id",
                         "“%1” is bypassed because its plugin (%2) is not installed.", m.name, m.detail);
        case Project.PluginLoadFailed:
            return i18nc("@info %1 is a device name, %2 a plugin id and message which may be in English",
                         "“%1” is bypassed because its plugin would not load (%2).", m.name, m.detail);
        case Project.PluginCrashed:
            return i18nc("@info %1 is a device name, %2 a plugin id",
                         "The plugin of “%1” (%2) stopped working and is bypassed.", m.name, m.detail);
        case Project.PluginStateNotSaved:
            return i18nc("@info %1 is a device name, %2 a message which may be in English",
                         "“%1” did not hand over its plugin settings, so the project keeps the ones saved before (%2).", m.name, m.detail);
        case Project.PluginEditorFailed:
            return i18nc("@info %1 is a device name, %2 a message which may be in English",
                         "The plugin interface of “%1” could not be opened (%2).", m.name, m.detail);
        case Project.CannotStream:
            return i18nc("@info %1 is a file path, %2 a message which may be in English",
                         "“%1” could not be played from disk, so its clips are silent (%2).", m.path, m.detail);
        case Project.CannotRead:
            return i18nc("@info %1 is a file path", "“%1” could not be read, so its clips are silent.", m.path);
        case Project.NotesDropped:
            return i18nc("@info %1 is a track name",
                         "Changes to the notes on “%1” have not reached the audio engine yet.", m.name);
        case Project.ClipsDropped:
            return i18nc("@info %1 is a track name",
                         "Changes to the clips on “%1” have not reached the audio engine yet.", m.name);
        case Project.NoInstrumentTrack:
            return i18nc("@info", "Arm an audio track to record audio, or add an instrument track to record MIDI on.");
        case Project.EngineNotRunning:
            return i18nc("@info", "The audio engine is not running.");
        case Project.NoMidiPlayer:
            return i18nc("@info", "The track is not ready to record yet; try again in a moment.");
        case Project.MidiInputDropped:
            return i18nc("@info", "Some MIDI input arrived too fast to be recorded, so the take may be missing notes.");
        case Project.NothingRecorded:
            return i18nc("@info", "The take was outside the song, so nothing was added.");
        case Project.Exported:
            return i18ncp("@info %2 is a file path", "Exported %1 second of audio to “%2”.",
                          "Exported %1 seconds of audio to “%2”.", m.number, m.path);
        case Project.ExportCancelled:
            return i18nc("@info", "The export was stopped, so no file was written.");
        case Project.EmptySong:
            return i18nc("@info", "The song has no clips yet.");
        case Project.NoLoop:
            return i18nc("@info", "No loop is set; choose another range or set a loop first.");
        case Project.EmptyRange:
            return i18nc("@info", "The range to export is empty.");
        case Project.ExportBusy:
            return i18nc("@info", "Another export is still running.");
        case Project.DiskTooSlow:
            return i18nc("@info", "Audio files could not be read from disk fast enough, so the export would have had gaps.");
        case Project.PluginTooSlow:
            return i18nc("@info", "A plugin did not keep up, so the export would have had gaps.");
        case Project.ExportPluginCrashed:
            return i18nc("@info %1 is a device name",
                         "The plugin of “%1” stopped working, so the export would have been without it.", m.name);
        case Project.LatencyChanged:
            return i18nc("@info", "A plugin changed its delay during the export, which would have shifted the audio. Try again.");
        case Project.WrongExtension:
            return i18nc("@info %1 is a file name extension like wav",
                         "The file name ends in “.%1”, which is not the chosen format.", m.detail);
        case Project.EncodeFailed:
            return i18nc("@info %1 is a message which may be in English",
                         "The audio could not be written (%1).", m.detail);
        case Project.CannotRecordTo:
            return i18nc("@info %1 is a file path, %2 a message which may be in English",
                         "“%1” could not be created (%2).", m.path, m.detail);
        case Project.NoSuchInput:
            return i18nc("@info %1 is a track name",
                         "“%1” records from an input the audio system does not have, so it was not recorded.", m.name);
        case Project.TrackNotReady:
            return i18nc("@info %1 is a track name",
                         "“%1” is not ready to record yet, so it was not recorded; try again in a moment.", m.name);
        case Project.RecordingStopped:
            return i18nc("@info %1 is a track name, %2 a message which may be in English",
                         "Recording “%1” stopped early, possibly because the disk is full. What was recorded before is kept (%2).", m.name, m.detail);
        case Project.AudioDropped:
            return i18ncp("@info %1 is a number of samples, %2 a track name",
                          "The disk was too slow and %1 sample of “%2” was lost, so the rest of its take is slightly early.",
                          "The disk was too slow and %1 samples of “%2” were lost, so the rest of its take is slightly early.",
                          m.number, m.name);
        case Project.PositionJumped:
            return i18nc("@info %1 is a track name",
                         "The play position jumped while recording “%1”; the take carries on straight past the jump.", m.name);
        case Project.TrackGoneWhileRecording:
            return i18nc("@info %1 is a track name",
                         "“%1” was deleted while recording, so its take was not kept.", m.name);
        case Project.RecordXruns:
            return i18ncp("@info",
                          "The audio system missed %1 cycle while recording, so the take may have a gap or click.",
                          "The audio system missed %1 cycles while recording, so the takes may have gaps or clicks.",
                          m.number);
        case Project.AlreadyOpen:
            return i18nc("@info", "It is open in another Kadence window.");
        }
        return "";
    }

    // What happened, followed by why. "" for an empty map.
    function text(m: var): string {
        if (m === undefined || m.context === undefined) {
            return "";
        }
        const why = reason(m);
        switch (m.context) {
        case Project.OpenContext:
            return i18nc("@info %1 is the reason, a full sentence", "Could not open the project. %1", why);
        case Project.SaveContext:
            // The project was saved, only without that plugin's latest settings.
            if (m.reason === Project.PluginStateNotSaved) {
                return why;
            }
            return i18nc("@info %1 is the reason, a full sentence", "Could not save the project. %1", why);
        case Project.AutosaveContext:
            return i18nc("@info %1 is the reason, a full sentence",
                         "Could not keep a backup of your unsaved changes. %1", why);
        case Project.ImportContext:
            if (m.fileName === "") {
                return i18nc("@info %1 is the reason, a full sentence", "Could not add the file. %1", why);
            }
            return i18nc("@info %1 is a file name, %2 the reason, a full sentence",
                         "Could not add “%1”. %2", m.fileName, why);
        case Project.EditContext:
            return i18nc("@info %1 is the reason, a full sentence", "That edit could not be made. %1", why);
        case Project.PlaybackContext:
            return i18nc("@info %1 is the reason, a full sentence", "Playback may be incomplete. %1", why);
        case Project.CollectContext:
            return i18nc("@info %1 is the reason, a full sentence",
                         "Could not copy the audio files into the project. %1", why);
        case Project.RecoverContext:
            return i18nc("@info %1 is the reason, a full sentence", "Could not recover the unsaved changes. %1", why);
        case Project.ExportContext:
            switch (m.reason) {
            case Project.Exported:
            case Project.ExportCancelled:
                return why;
            }
            return i18nc("@info %1 is the reason, a full sentence", "Could not export the song. %1", why);
        case Project.RecordContext:
            switch (m.reason) {
            case Project.NoInstrumentTrack:
            case Project.EngineNotRunning:
            case Project.NoMidiPlayer:
                return i18nc("@info %1 is the reason, a full sentence", "Could not start recording. %1", why);
            case Project.CannotRecordTo:
                return i18nc("@info %1 is the reason, a full sentence", "Could not record. %1", why);
            case Project.EditRefused:
                return i18nc("@info %1 is the reason, a full sentence", "The take could not be added. %1", why);
            }
            // The reason says it all: what the take may lack, or why it
            // was not added.
            return why;
        }
        return "";
    }
}
