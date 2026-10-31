// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// The settings dialog: one tab per group.
Kirigami.ScrollablePage {
    id: page

    title: i18nc("@title", "Settings")

    // For the self test.
    property alias currentTab: tabs.currentIndex

    header: QQC2.TabBar {
        id: tabs

        QQC2.TabButton {
            text: i18nc("@title:tab", "General")
            icon.name: "configure"
        }
        QQC2.TabButton {
            text: i18nc("@title:tab", "Audio")
            icon.name: "audio-card"
        }
        QQC2.TabButton {
            text: i18nc("@title:tab", "MIDI")
            icon.name: "audio-midi"
        }
    }

    StackLayout {
        currentIndex: tabs.currentIndex

        GeneralSettings {}
        // The audio and plugin forms share one label column.
        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing

            AudioSettings {
                id: audio
                Layout.fillWidth: true
            }

            PluginSettings {
                Layout.fillWidth: true
                twinFormLayouts: [audio.form]
            }
        }
        MidiSettings {}
    }
}
