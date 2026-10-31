// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Which MIDI sources Kadence listens to. Each one used gets an input of its
// own, so a track can listen to just that device. Changes apply at once.
ColumnLayout {
    id: settings

    spacing: Kirigami.Units.largeSpacing

    Kirigami.InlineMessage {
        Layout.fillWidth: true
        type: Kirigami.MessageType.Information
        visible: Engine.backendKind === Engine.SilentBackend
        text: i18nc("@info", "MIDI devices are reached through the audio server. Start PipeWire with its JACK support (or JACK) to use them.")
    }

    Kirigami.FormLayout {
        Layout.fillWidth: true

        QQC2.CheckBox {
            Kirigami.FormData.label: i18nc("@label", "New devices:")
            text: i18nc("@option:check", "Use MIDI devices when they are plugged in")
            checked: AppSettings.useNewMidiDevices
            onToggled: AppSettings.chooseUseNewMidiDevices(checked)
        }

        Kirigami.Separator {
            Kirigami.FormData.isSection: true
            Kirigami.FormData.label: i18nc("@title:group", "Inputs")
        }

        Repeater {
            model: AppSettings.midiSourceNames

            delegate: QQC2.CheckBox {
                required property int index
                required property string modelData

                readonly property bool present: AppSettings.midiSourcePresent[index] ?? false
                readonly property bool physical: AppSettings.midiSourcePhysical[index] ?? false

                Kirigami.FormData.label: index === 0 ? i18nc("@label", "Listen to:") : ""
                text: {
                    const label = AppSettings.midiSourceLabels[index] ?? modelData;
                    if (!present) {
                        return i18nc("@option:check %1 is a MIDI device", "%1 (unplugged)", label);
                    }
                    return physical ? label : i18nc("@option:check %1 is a program's MIDI output", "%1 (program)", modelData);
                }
                checked: AppSettings.midiSourceUsed[index] ?? false
                onToggled: AppSettings.useMidiSource(modelData, checked)
                QQC2.ToolTip.text: modelData
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            }
        }

        QQC2.Label {
            Kirigami.FormData.label: i18nc("@label", "Listen to:")
            visible: AppSettings.midiSourceNames.length === 0
            text: i18nc("@info", "No MIDI devices found")
            enabled: false
        }

        QQC2.Label {
            Layout.fillWidth: true
            Layout.maximumWidth: Kirigami.Units.gridUnit * 20
            text: i18nc("@info", "Tracks set to listen to all inputs hear every device used here. Choose one device or channel in a track's MIDI input menu.")
            wrapMode: Text.Wrap
            font: Kirigami.Theme.smallFont
            opacity: 0.7
        }
    }
}
