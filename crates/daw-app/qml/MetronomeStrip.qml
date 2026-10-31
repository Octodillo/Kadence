// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// The metronome in the mixer: on or off, its level and count-in, the same
// settings as the transport bar's. Laid out like a ChannelStrip so its
// fader lines up with the others; the click always plays through the
// master.
Item {
    id: strip

    property real stripWidth: Kirigami.Units.gridUnit * 7
    property real slotsHeight: Kirigami.Units.gridUnit * 11

    implicitWidth: stripWidth
    width: implicitWidth

    // MetronomeSettings::default() in daw-session.
    readonly property real defaultLevel: -6

    function setLevel(db: real, save: bool): void {
        Project.setMetronomeLevel(Math.round(db * 2) / 2, save);
    }

    Rectangle {
        anchors.fill: parent
        color: Kirigami.Theme.backgroundColor
    }
    Kirigami.Separator {
        anchors {
            top: parent.top
            bottom: parent.bottom
            right: parent.right
        }
    }

    ColumnLayout {
        anchors {
            fill: parent
            margins: Kirigami.Units.smallSpacing
            rightMargin: Kirigami.Units.smallSpacing + 1
        }
        spacing: Kirigami.Units.smallSpacing

        Kirigami.Heading {
            Layout.fillWidth: true
            level: 5
            text: i18nc("@title metronome channel strip", "Metronome")
            elide: Text.ElideRight
            horizontalAlignment: Text.AlignHCenter
            opacity: Project.metronome ? 1 : 0.6
        }
        QQC2.Label {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            elide: Text.ElideRight
            font: Kirigami.Theme.smallFont
            color: Kirigami.Theme.disabledTextColor
            text: i18nc("@label strip type", "Click")
        }

        // Where the other strips have their effects.
        ColumnLayout {
            Layout.fillWidth: true
            // A layout fills by default; this one is as high as the
            // others' effects box.
            Layout.fillHeight: false
            Layout.preferredHeight: strip.slotsHeight
            Layout.minimumHeight: Kirigami.Units.gridUnit * 4
            spacing: Kirigami.Units.smallSpacing
            Kirigami.ListSectionHeader {
                Layout.fillWidth: true
                text: i18nc("@title:group", "Count-in")
            }
            QQC2.ComboBox {
                Layout.fillWidth: true
                model: [
                    i18nc("@item:inlistbox no count-in", "None"),
                    i18ncp("@item:inlistbox count-in length", "%1 bar", "%1 bars", 1),
                    i18ncp("@item:inlistbox count-in length", "%1 bar", "%1 bars", 2)
                ]
                currentIndex: Project.countInBars
                onActivated: index => Project.setCountIn(index)
                Accessible.name: i18nc("@label:listbox", "Count-in before recording")
                QQC2.ToolTip.text: i18nc("@info:tooltip", "Bars the metronome counts in before recording")
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            }
            Item {
                Layout.fillHeight: true
            }
        }

        // The other strips' resize line and pan, kept empty so the faders
        // line up.
        Item {
            Layout.fillWidth: true
            implicitHeight: Kirigami.Units.smallSpacing * 2
            Layout.topMargin: -Kirigami.Units.smallSpacing
            Layout.bottomMargin: -Kirigami.Units.smallSpacing
            Kirigami.Separator {
                anchors.verticalCenter: parent.verticalCenter
                width: parent.width
            }
        }
        QQC2.Label {
            text: " "
            font: Kirigami.Theme.smallFont
        }
        QQC2.Slider {
            Layout.fillWidth: true
            opacity: 0
            enabled: false
            Accessible.ignored: true
        }

        QQC2.Slider {
            id: fader
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.minimumHeight: Kirigami.Units.gridUnit * 6
            orientation: Qt.Vertical
            from: -40
            to: 6
            value: Project.metronomeVolume
            // Saved once the handle is let go, not on every move.
            onMoved: strip.setLevel(value, !pressed)
            onPressedChanged: {
                if (!pressed) {
                    strip.setLevel(value, true);
                }
            }
            Accessible.name: i18nc("@info:accessible", "Metronome level")
            QQC2.ToolTip.text: i18nc("@info:tooltip %1 is a level in decibels", "Metronome level (double-click for %1 dB)",
                                     strip.defaultLevel.toLocaleString(Qt.locale(), "f", 1))
            QQC2.ToolTip.visible: hovered && !pressed
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            TapHandler {
                // After the slider's own release (see ChannelStrip.later).
                onDoubleTapped: Qt.callLater(() => strip.setLevel(strip.defaultLevel, true))
            }
        }
        QQC2.Label {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            font.features: { "tnum": 1 }
            text: i18nc("@label decibels", "%1 dB", Number(fader.value).toLocaleString(Qt.locale(), "f", 1))
        }

        // Where the other strips have mute and solo.
        QQC2.Button {
            Layout.fillWidth: true
            checkable: true
            checked: Project.metronome
            icon.name: "chronometer"
            text: Project.metronome ? i18nc("@action:button metronome is on", "On")
                                    : i18nc("@action:button metronome is off", "Off")
            Accessible.name: i18nc("@action:button", "Metronome")
            QQC2.ToolTip.text: i18nc("@info:tooltip", "Metronome: click on every beat while playing")
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            onToggled: Project.setMetronomeEnabled(checked)
        }

        // Where the other strips choose their output.
        QQC2.Label {
            Layout.fillWidth: true
            Layout.minimumHeight: Kirigami.Units.gridUnit * 2
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
            font: Kirigami.Theme.smallFont
            color: Kirigami.Theme.disabledTextColor
            text: i18nc("@label the metronome plays through the master bus", "Through Master")
        }
    }
}
