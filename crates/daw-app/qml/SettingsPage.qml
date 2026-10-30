// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Kirigami.ScrollablePage {
    title: i18nc("@title", "Settings")

    ColumnLayout {
        spacing: Kirigami.Units.largeSpacing

        AudioSettings {
            Layout.fillWidth: true
        }

        PluginSettings {
            Layout.fillWidth: true
        }
    }
}
