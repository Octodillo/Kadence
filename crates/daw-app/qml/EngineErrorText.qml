// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

import io.github.octodillo.daw

// Words the engine's current error (Engine.errorContext and friends) in the
// user's language. The engine only sends data, so every message is here.
QtObject {
    // True when there is an error to show.
    readonly property bool active: Engine.errorContext !== Engine.NoError

    // Why it happened, as a full sentence.
    readonly property string reason: {
        switch (Engine.errorKind) {
        case Engine.UnknownDevice:
            return i18nc("@info %1 is an audio device id", "The audio device “%1” is not available.",
                         Engine.errorDetail);
        case Engine.UnsupportedSampleRate:
            return i18nc("@info %1 is a number", "The audio device does not support a sample rate of %1 Hz.",
                         Engine.errorNumber.toLocaleString(Qt.locale(), "f", 0));
        case Engine.UnsupportedBufferSize:
            return i18ncp("@info", "The audio device does not support a buffer size of %1 sample.",
                          "The audio device does not support a buffer size of %1 samples.",
                          Engine.errorNumber);
        case Engine.BackendError:
            return i18nc("@info %1 is a message from the audio system, which may be in English",
                         "The audio system reported: %1", Engine.errorDetail);
        case Engine.InvalidDeviceChoice:
            return i18nc("@info", "The chosen audio device is not in the list.");
        case Engine.InvalidSampleRate:
            return i18nc("@info", "The chosen sample rate is not valid.");
        case Engine.InvalidBufferSize:
            return i18nc("@info", "The chosen buffer size is not valid.");
        }
        return "";
    }

    // What happened, followed by why.
    readonly property string text: {
        switch (Engine.errorContext) {
        case Engine.ApplyingSettings:
            return i18nc("@info %1 is the reason, a full sentence", "Could not change the audio settings. %1",
                         reason);
        case Engine.StartupFallback:
            return i18nc("@info %1 is the reason, a full sentence",
                         "Your saved audio settings could not be used, so Kadence started with other ones. %1",
                         reason);
        case Engine.StartupFailed:
            return i18nc("@info %1 is the reason, a full sentence", "Audio could not be started. %1", reason);
        case Engine.AudioFailed:
            return i18nc("@info %1 is the reason, a full sentence", "Audio is not working. %1", reason);
        }
        return "";
    }
}
