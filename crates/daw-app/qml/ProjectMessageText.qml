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
            return i18nc("@info %1 is a folder path",
                         "Unsaved changes from an earlier session are waiting in “%1”.", m.path);
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
        }
        return "";
    }
}
