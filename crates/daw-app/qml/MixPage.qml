// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// The mixer: one channel strip per track and bus, master on the right.
Kirigami.Page {
    id: page

    property list<Kirigami.Action> transportActions

    title: i18nc("@title", "Mix")
    // QML list literals can't spread lists, so name each entry.
    actions: [page.transportActions[0], page.writeAction]

    readonly property Kirigami.Action writeAction: Kirigami.Action {
        icon.name: "media-record-symbolic"
        text: i18nc("@action:intoolbar", "Write Automation")
        tooltip: i18nc("@info:tooltip", "Record fader, pan and send moves as automation while playing")
        checkable: true
        checked: Mixer.writeAutomation
        onToggled: Mixer.setWriting(checked)
    }
    padding: 0

    Kirigami.Theme.colorSet: Kirigami.Theme.View
    Kirigami.Theme.inherit: false

    // Meters run only while the page is on screen.
    Timer {
        interval: 33
        repeat: true
        running: page.visible
        property double last: Date.now()
        onRunningChanged: last = Date.now()
        onTriggered: {
            const now = Date.now();
            Mixer.tick(Project.playing, (now - last) / 1000);
            last = now;
        }
    }

    // Every strip's width and effects box height.
    readonly property LayoutSize stripWidth: LayoutSize {
        key: "MixerStripWidth"
        defaultUnits: 7
        minimumUnits: 5
        maximumUnits: 20
    }
    readonly property LayoutSize boxHeight: LayoutSize {
        key: "MixerSlotsHeight"
        defaultUnits: 11
        minimumUnits: 4
        maximumUnits: 60
    }

    background: Rectangle {
        color: Kirigami.Theme.backgroundColor
    }

    QQC2.ScrollView {
        anchors.fill: parent
        QQC2.ScrollBar.vertical.policy: QQC2.ScrollBar.AlwaysOff

        ListView {
            id: strips
            orientation: ListView.Horizontal
            model: Mixer
            spacing: 0
            boundsBehavior: Flickable.StopAtBounds
            clip: true

            // After the master, which it plays through.
            footer: MetronomeStrip {
                height: ListView.view.height
                visible: ListView.view.count > 0
                widthSize: page.stripWidth
                boxSize: page.boxHeight
                position: ListView.view.count
            }

            delegate: ChannelStrip {
                height: ListView.view.height
                widthSize: page.stripWidth
                boxSize: page.boxHeight
            }
        }
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - Kirigami.Units.gridUnit * 4
        visible: strips.count === 0
        icon.name: "view-media-equalizer"
        text: i18nc("@info:placeholder", "Nothing to mix yet")
        explanation: i18nc("@info:placeholder", "Each track gets a channel strip with volume, pan and effects here.")
    }
}
