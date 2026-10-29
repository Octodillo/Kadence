// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

// A top-level view (Arrange, Mix, Edit). Until each view is built it shows a
// placeholder explaining what will live there.
Kirigami.Page {
    id: page

    property list<Kirigami.Action> transportActions
    property alias placeholderIcon: placeholder.icon.name
    property alias placeholderText: placeholder.text
    property alias placeholderExplanation: placeholder.explanation

    actions: transportActions
    padding: 0

    Kirigami.PlaceholderMessage {
        id: placeholder
        anchors.centerIn: parent
        width: parent.width - Kirigami.Units.gridUnit * 4
    }
}
