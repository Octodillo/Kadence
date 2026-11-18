// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Dialogs

// A file dialog for picking audio files: it offers the formats the decoder
// reads.
FileDialog {
    nameFilters: [
        i18nc("@item:inlistbox", "Audio files (*.wav *.flac *.ogg *.oga *.mp3 *.aif *.aiff)"),
        i18nc("@item:inlistbox", "All files (*)")
    ]
}
