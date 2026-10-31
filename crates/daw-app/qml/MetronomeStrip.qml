// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// The metronome in the mixer: on or off, its level and count-in, the same
// settings as the transport bar's. A strip like the channels, so its fader
// lines up with theirs; the click always plays through the master.
StripFrame {
    id: strip

    title: i18nc("@title metronome channel strip", "Metronome")
    subtitle: i18nc("@label strip type", "Click")
    dimmed: !Project.metronome
    slack: fader.height - fader.Layout.minimumHeight

    // MetronomeSettings::default() in daw-session.
    readonly property real defaultLevel: -6

    function setLevel(db: real, save: bool): void {
        Project.setMetronomeLevel(Math.round(db * 2) / 2, save);
    }

    // The controls set their own values when used, which replaces a plain
    // binding; follow the project (the transport bar's controls) from here,
    // as TransportBar does.
    Connections {
        target: Project
        function onMetronomeVolumeChanged() { fader.value = Project.metronomeVolume; }
        function onCountInBarsChanged() { countIn.currentIndex = Project.countInBars; }
        function onMetronomeChanged() { onButton.checked = Project.metronome; }
    }

    // Where the channels have their effects.
    box: ColumnLayout {
        anchors.fill: parent
        spacing: Kirigami.Units.smallSpacing
        Kirigami.ListSectionHeader {
            Layout.fillWidth: true
            text: i18nc("@title:group", "Count-in")
        }
        QQC2.ComboBox {
            id: countIn
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

    // The channels' pan, kept empty so the faders line up.
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
            // After the slider's own release (see ChannelStrip.reset).
            onDoubleTapped: Qt.callLater(() => strip.setLevel(strip.defaultLevel, true))
        }
    }
    QQC2.Label {
        Layout.fillWidth: true
        horizontalAlignment: Text.AlignHCenter
        font.features: { "tnum": 1 }
        // As it is stored: in half decibels.
        text: i18nc("@label decibels", "%1 dB", Number(Math.round(fader.value * 2) / 2).toLocaleString(Qt.locale(), "f", 1))
    }

    // Where the other strips have mute and solo.
    QQC2.Button {
        id: onButton
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
