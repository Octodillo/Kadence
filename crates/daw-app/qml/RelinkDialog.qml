// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Lists the audio files the project's clips and samplers play that are not on disk
// (Project.missingCount), and lets the user point each at where it is now.
// Locating one also finds the other missing files in that folder by name.
Kirigami.Dialog {
    id: dialog

    // Emitted after a relink with how many files it found.
    signal relinked(int count)

    title: i18nc("@title:window", "Find Missing Audio Files")
    standardButtons: Kirigami.Dialog.Close
    preferredWidth: Kirigami.Units.gridUnit * 30
    padding: Kirigami.Units.largeSpacing

    // The missing file being located.
    property int locating: -1

    Connections {
        target: Project
        function onMissingCountChanged(): void {
            if (Project.missingCount === 0) {
                dialog.close();
            }
        }
    }

    AudioFileDialog {
        id: locateDialog
        title: i18nc("@title:window", "Locate Audio File")
        onAccepted: {
            const n = Project.relink(dialog.locating, selectedFile);
            dialog.locating = -1;
            dialog.relinked(n);
        }
        onRejected: dialog.locating = -1
    }

    ColumnLayout {
        spacing: Kirigami.Units.largeSpacing

        QQC2.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            text: i18ncp("@info",
                         "This audio file could not be found, so what plays it is silent. Locate it, or close this to leave it missing.",
                         "These %1 audio files could not be found, so what plays them is silent. Locate them, or close this to leave them missing.",
                         Project.missingCount)
        }

        Repeater {
            model: Project.missingFiles

            delegate: RowLayout {
                id: row
                required property int index
                required property string modelData
                readonly property string file: modelData

                Layout.fillWidth: true
                spacing: Kirigami.Units.smallSpacing

                Kirigami.Icon {
                    source: "audio-x-generic"
                    Layout.preferredWidth: Kirigami.Units.iconSizes.medium
                    Layout.preferredHeight: Kirigami.Units.iconSizes.medium
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0

                    QQC2.Label {
                        Layout.fillWidth: true
                        elide: Text.ElideMiddle
                        text: row.file.split("/").pop()
                    }
                    QQC2.Label {
                        Layout.fillWidth: true
                        elide: Text.ElideMiddle
                        font: Kirigami.Theme.smallFont
                        color: Kirigami.Theme.disabledTextColor
                        readonly property int clips: Project.missingClipCount(row.index)
                        readonly property int samplers: Project.missingSamplerCount(row.index)
                        text: samplers === 0
                              ? i18ncp("@info %2 is a file path", "Was at %2, used by %1 clip",
                                       "Was at %2, used by %1 clips", clips, row.file)
                              : clips === 0
                              ? i18ncp("@info %2 is a file path", "Was at %2, used by %1 sampler",
                                       "Was at %2, used by %1 samplers", samplers, row.file)
                              : i18nc("@info %1 is a file path, %2 a number of clips, %3 of samplers",
                                      "Was at %1, used by %2 and %3", row.file,
                                      i18ncp("@info used by…", "%1 clip", "%1 clips", clips),
                                      i18ncp("@info used by…", "%1 sampler", "%1 samplers", samplers))
                    }
                }

                QQC2.Button {
                    text: i18nc("@action:button", "Locate…")
                    icon.name: "document-open"
                    onClicked: {
                        dialog.locating = row.index;
                        locateDialog.open();
                    }
                }
            }
        }
    }
}
