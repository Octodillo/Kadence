// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Picks the live MIDI an instrument track listens to: every input or one
// device, and every channel or one.
QQC2.ToolButton {
    id: button

    required property int trackIndex
    property int revision: Project.revision

    readonly property string device: revision >= 0 ? Project.trackMidiDevice(trackIndex) : ""
    readonly property int channel: revision >= 0 ? Project.trackMidiChannel(trackIndex) : -1
    // A device the track names that is not in use (unplugged and turned off,
    // or from another computer).
    readonly property bool deviceMissing: device !== "" && !AppSettings.midiInputNames.includes(device)

    readonly property string deviceText: device === "" ? i18nc("@item:inmenu MIDI input", "All Inputs")
                                                       : AppSettings.midiLabel(device)
    readonly property string channelText: channel < 0 ? i18nc("@item:inmenu MIDI channel", "All Channels")
                                                      : i18nc("@item:inmenu %1 is a MIDI channel number", "Channel %1", channel + 1)

    icon.name: "audio-midi"
    icon.color: deviceMissing ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.textColor
    display: QQC2.AbstractButton.IconOnly
    text: i18nc("@action:button", "MIDI Input")
    down: menu.visible
    onClicked: menu.popup(button, 0, button.height)

    QQC2.ToolTip.text: i18nc("@info:tooltip %1 is the MIDI input, %2 the channel", "MIDI input: %1, %2", deviceText, channelText)
    QQC2.ToolTip.visible: hovered && !menu.visible
    QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay

    // Closes the menu, if open.
    function dismiss(): void {
        menu.dismiss();
    }

    function choose(newDevice: string, newChannel: int): void {
        Project.setTrackMidiInput(trackIndex, newDevice, newChannel);
    }

    QQC2.Menu {
        id: menu

        QQC2.MenuItem {
            text: i18nc("@item:inmenu MIDI input", "All Inputs")
            checkable: true
            checked: button.device === ""
            onTriggered: button.choose("", button.channel)
        }

        // A Repeater can't add items to a Menu; insert them after "All
        // Inputs".
        Instantiator {
            model: AppSettings.midiInputNames
            delegate: QQC2.MenuItem {
                required property int index
                required property string modelData
                text: AppSettings.midiInputLabels[index] ?? modelData
                checkable: true
                checked: button.device === modelData
                onTriggered: button.choose(modelData, button.channel)
            }
            onObjectAdded: (index, object) => menu.insertItem(index + 1, object)
            onObjectRemoved: (index, object) => menu.removeItem(object)
        }

        QQC2.MenuItem {
            visible: button.deviceMissing
            height: visible ? implicitHeight : 0
            text: i18nc("@item:inmenu %1 is a MIDI device that is not in use", "%1 (not in use)", AppSettings.midiLabel(button.device))
            checkable: true
            checked: true
        }

        QQC2.MenuSeparator {}

        QQC2.Menu {
            id: channelMenu
            title: button.channelText

            QQC2.MenuItem {
                text: i18nc("@item:inmenu MIDI channel", "All Channels")
                checkable: true
                checked: button.channel < 0
                onTriggered: button.choose(button.device, -1)
            }

            QQC2.MenuSeparator {}

            Instantiator {
                model: 16
                delegate: QQC2.MenuItem {
                    required property int index
                    text: i18nc("@item:inmenu %1 is a MIDI channel number", "Channel %1", index + 1)
                    checkable: true
                    checked: button.channel === index
                    onTriggered: button.choose(button.device, index)
                }
                // After "All Channels" and the separator.
                onObjectAdded: (index, object) => channelMenu.insertItem(index + 2, object)
                onObjectRemoved: (index, object) => channelMenu.removeItem(object)
            }
        }
    }
}
