// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// The controls of track `track` that have no automation lane yet; picking
// one adds its lane and shows the track's lanes.
QQC2.Menu {
    id: menu

    required property int track
    // Read when the menu opens, so it lists what is current.
    property int targets: 0

    title: i18nc("@title:menu", "Add Automation Lane")
    icon.name: "list-add"

    onAboutToShow: targets = Automation.targetCount(track)

    LaneName {
        id: laneName
    }

    QQC2.MenuItem {
        visible: menu.targets === 0
        height: visible ? implicitHeight : 0
        enabled: false
        text: i18nc("@item:inmenu", "Every control has a lane")
    }

    Instantiator {
        model: menu.targets
        delegate: QQC2.MenuItem {
            required property int index
            text: laneName.text(Automation.targetKind(menu.track, index),
                                Automation.targetDetail(menu.track, index),
                                Automation.targetParam(menu.track, index))
            onTriggered: Automation.addLane(menu.track, index)
        }
        onObjectAdded: (index, object) => menu.insertItem(index + 1, object)
        onObjectRemoved: (index, object) => menu.removeItem(object)
    }
}
