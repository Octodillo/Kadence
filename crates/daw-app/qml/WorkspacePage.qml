// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

// A top-level view (Arrange, Mix, Edit). Until each view is built it shows a
// placeholder explaining what will live there.
Kirigami.Page {
    id: page

    property list<Kirigami.Action> transportActions
    property string placeholderIcon
    property string placeholderText
    property string placeholderExplanation

    actions: transportActions
    padding: 0

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - Kirigami.Units.gridUnit * 4
        icon.name: page.placeholderIcon
        text: page.placeholderText
        explanation: page.placeholderExplanation
    }
}
