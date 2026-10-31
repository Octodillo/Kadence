// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// One mixer channel: instrument, inserts, sends, pan, fader with meter, mute,
// solo and where its output goes.
Item {
    id: strip

    required property int index
    required property string name
    required property string kind
    required property double volumeDb
    required property double pan
    required property bool muted
    required property bool soloed
    required property bool silenced
    required property bool hasInstrument
    required property string instrument
    required property int instrumentChoice
    required property list<string> inserts
    required property list<bool> insertsEnabled
    required property bool insertMeters
    required property list<string> sends
    required property list<real> sendLevels
    required property list<bool> sendPreFader
    required property list<string> sendChoices
    required property list<string> outputChoices
    required property int outputIndex

    required property double levelLeft
    required property double levelRight
    required property double holdLeft
    required property double holdRight
    required property double gainReduction

    readonly property bool isMaster: kind === "master"
    readonly property string masterName: i18nc("@title name of the master bus", "Master")

    implicitWidth: Kirigami.Units.gridUnit * 7
    width: implicitWidth

    function dbText(db: real): string {
        if (db <= -60) {
            return i18nc("@label fader at silence", "−∞");
        }
        const rounded = Math.round(db * 10) / 10;
        return i18nc("@label decibels", "%1 dB", (rounded > 0 ? "+" : "") + rounded.toLocaleString(Qt.locale(), "f", 1));
    }

    function panText(p: real): string {
        const pct = Math.round(Math.abs(p) * 100);
        if (pct === 0) {
            return i18nc("@label pan centre", "C");
        }
        return p < 0 ? i18nc("@label pan left, percent", "L%1", pct) : i18nc("@label pan right, percent", "R%1", pct);
    }

    Rectangle {
        anchors.fill: parent
        color: strip.isMaster ? Kirigami.Theme.alternateBackgroundColor : Kirigami.Theme.backgroundColor
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

        // Name
        Kirigami.Heading {
            Layout.fillWidth: true
            level: 5
            // The master is unnamed unless the user renamed it.
            text: strip.name || (strip.isMaster ? strip.masterName : "")
            elide: Text.ElideRight
            horizontalAlignment: Text.AlignHCenter
            opacity: strip.silenced ? 0.6 : 1
        }
        QQC2.Label {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            font: Kirigami.Theme.smallFont
            color: Kirigami.Theme.disabledTextColor
            text: strip.kind === "bus" ? i18nc("@label strip type", "Bus")
                : strip.isMaster ? i18nc("@label strip type", "Main output")
                : i18nc("@label strip type", "Track")
        }

        // Effects and sends scroll in a box of the same height on every
        // strip, so pan, faders and buttons line up across the mixer.
        QQC2.ScrollView {
            id: slots
            Layout.fillWidth: true
            Layout.preferredHeight: Kirigami.Units.gridUnit * 11
            Layout.minimumHeight: Kirigami.Units.gridUnit * 4
            QQC2.ScrollBar.horizontal.policy: QQC2.ScrollBar.AlwaysOff
            contentWidth: availableWidth

            ColumnLayout {
                width: slots.availableWidth
                spacing: Kirigami.Units.smallSpacing

                // Instrument, on instrument tracks
                Kirigami.ListSectionHeader {
                    Layout.fillWidth: true
                    text: i18nc("@title:group", "Instrument")
                    visible: strip.hasInstrument
                }
                InstrumentButton {
                    Layout.fillWidth: true
                    visible: strip.hasInstrument
                    instrument: strip.instrument
                    currentChoice: strip.instrumentChoice
                    onChosen: (choice, revision) => Mixer.setInstrument(strip.index, choice, revision)
                    onSettingsRequested: DevicePanel.open(Mixer.instrumentDevice(strip.index))
                }

                // Inserts
                Kirigami.ListSectionHeader {
                    Layout.fillWidth: true
                    text: i18nc("@title:group effect slots", "Effects")
                }
                Repeater {
                    model: strip.inserts
                    delegate: RowLayout {
                        id: insertRow
                        required property int index
                        required property string modelData
                        Layout.fillWidth: true
                        spacing: 0
                        // Opens the device panel; dimmed while bypassed.
                        QQC2.ItemDelegate {
                            Layout.fillWidth: true
                            text: insertRow.modelData
                            opacity: strip.insertsEnabled[insertRow.index] === false ? 0.5 : 1
                            onClicked: DevicePanel.open(Mixer.insertDevice(strip.index, insertRow.index))
                            QQC2.ToolTip.text: i18nc("@info:tooltip", "Show the settings of %1", insertRow.modelData)
                            QQC2.ToolTip.visible: hovered
                            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                        }
                        QQC2.ToolButton {
                            icon.name: "list-remove"
                            display: QQC2.AbstractButton.IconOnly
                            text: i18nc("@action:button", "Remove %1", insertRow.modelData)
                            QQC2.ToolTip.text: text
                            QQC2.ToolTip.visible: hovered
                            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                            onClicked: Mixer.removeInsert(strip.index, insertRow.index)
                        }
                    }
                }
                QQC2.ToolButton {
                    Layout.fillWidth: true
                    icon.name: "list-add"
                    text: i18nc("@action:button", "Add Effect")
                    onClicked: effectMenu.popup()
                    QQC2.Menu {
                        id: effectMenu
                        Repeater {
                            model: Mixer.effectNames
                            delegate: QQC2.MenuItem {
                                required property int index
                                required property string modelData
                                text: modelData
                                onTriggered: Mixer.addInsert(strip.index, index)
                            }
                        }
                        QQC2.MenuSeparator {}
                        QQC2.MenuItem {
                            icon.name: "preferences-plugin"
                            text: i18nc("@action:inmenu", "Browse Plugins…")
                            onTriggered: {
                                // The row now: the delegate may be gone by the time a
                                // plugin is chosen.
                                const row = strip.index;
                                applicationWindow().browsePlugins(2, (format, pluginId, name) => {
                                    Mixer.addPluginInsert(row, format, pluginId, name);
                                });
                            }
                        }
                    }
                }
                // Gain reduction of the strip's compressor.
                RowLayout {
                    Layout.fillWidth: true
                    visible: strip.insertMeters
                    QQC2.Label {
                        text: i18nc("@label gain reduction, short", "GR")
                        font: Kirigami.Theme.smallFont
                        QQC2.ToolTip.text: i18nc("@info:tooltip", "Gain reduction")
                        QQC2.ToolTip.visible: grHover.hovered
                        HoverHandler { id: grHover }
                    }
                    GainReductionMeter {
                        Layout.fillWidth: true
                        reduction: Math.max(0, strip.gainReduction)
                    }
                }

                // Sends
                Kirigami.ListSectionHeader {
                    Layout.fillWidth: true
                    text: i18nc("@title:group", "Sends")
                    visible: !strip.isMaster
                }
                Repeater {
                    model: strip.sends
                    delegate: ColumnLayout {
                        id: sendRow
                        required property int index
                        required property string modelData
                        // Sends go to buses or the master; only the master is
                        // unnamed by default.
                        readonly property string target: modelData || strip.masterName
                        Layout.fillWidth: true
                        spacing: 0
                        RowLayout {
                            Layout.fillWidth: true
                            spacing: 0
                            QQC2.Label {
                                Layout.fillWidth: true
                                text: sendRow.target
                                elide: Text.ElideRight
                                font: Kirigami.Theme.smallFont
                                leftPadding: Kirigami.Units.smallSpacing
                            }
                            QQC2.ToolButton {
                                icon.name: "list-remove"
                                display: QQC2.AbstractButton.IconOnly
                                text: i18nc("@action:button", "Remove send to %1", sendRow.target)
                                QQC2.ToolTip.text: text
                                QQC2.ToolTip.visible: hovered
                                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                                onClicked: Mixer.removeSend(strip.index, sendRow.index)
                            }
                        }
                        RowLayout {
                            Layout.fillWidth: true
                            spacing: 0
                            QQC2.Slider {
                                Layout.fillWidth: true
                                from: -60
                                to: 6
                                value: strip.sendLevels[sendRow.index] ?? -60
                                onMoved: Mixer.setSendLevel(strip.index, sendRow.index, value)
                                onPressedChanged: Mixer.touch(strip.index, "send", sendRow.index, pressed)
                                QQC2.ToolTip.text: i18nc("@info:tooltip send level to bus", "Send to %1: %2", sendRow.target, strip.dbText(value))
                                QQC2.ToolTip.visible: hovered || pressed
                                Accessible.name: i18nc("@label:slider", "Send to %1", sendRow.target)
                            }
                            // Pre-fader sends ignore the fader: for monitor
                            // mixes. Post-fader (the default) follows it.
                            QQC2.ToolButton {
                                checkable: true
                                checked: strip.sendPreFader[sendRow.index] ?? false
                                text: i18nc("@action:button short for pre-fader send", "Pre")
                                font: Kirigami.Theme.smallFont
                                Accessible.name: i18nc("@action:button", "Send to %1 before the fader", sendRow.target)
                                QQC2.ToolTip.text: checked
                                    ? i18nc("@info:tooltip", "Sends before the fader; click to send after it")
                                    : i18nc("@info:tooltip", "Sends after the fader; click to send before it")
                                QQC2.ToolTip.visible: hovered
                                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                                onToggled: Mixer.setSendPreFader(strip.index, sendRow.index, checked)
                            }
                        }
                    }
                }
                // Only buses this strip doesn't already send to, and that
                // don't feed it, so a send can never loop back.
                QQC2.ToolButton {
                    Layout.fillWidth: true
                    visible: !strip.isMaster
                    enabled: strip.sendChoices.length > 0
                    icon.name: "list-add"
                    text: i18nc("@action:button", "Add Send")
                    QQC2.ToolTip.text: i18nc("@info:tooltip", "Send this channel to a bus")
                    QQC2.ToolTip.visible: hovered
                    QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                    onClicked: sendMenu.popup()
                    QQC2.Menu {
                        id: sendMenu
                        Repeater {
                            model: strip.sendChoices
                            delegate: QQC2.MenuItem {
                                required property int index
                                required property string modelData
                                text: modelData
                                onTriggered: Mixer.addSend(strip.index, index)
                            }
                        }
                    }
                }

            }
        }


        // Pan
        Kirigami.Separator {
            Layout.fillWidth: true
        }
        // Master has no pan or mute/solo; their space stays empty so its
        // fader lines up with the others.
        RowLayout {
            Layout.fillWidth: true
            opacity: strip.isMaster ? 0 : 1
            QQC2.Label {
                Layout.fillWidth: true
                text: i18nc("@label:slider", "Pan")
                font: Kirigami.Theme.smallFont
            }
            QQC2.Label {
                text: strip.panText(panSlider.value)
                font: Kirigami.Theme.smallFont
            }
        }
        QQC2.Slider {
            id: panSlider
            Layout.fillWidth: true
            opacity: strip.isMaster ? 0 : 1
            enabled: !strip.isMaster
            Accessible.ignored: strip.isMaster
            from: -1
            to: 1
            value: strip.pan
            onMoved: Mixer.setPan(strip.index, value)
            onPressedChanged: Mixer.touch(strip.index, "pan", -1, pressed)
            Accessible.name: i18nc("@label:slider", "Pan")
            QQC2.ToolTip.text: i18nc("@info:tooltip", "Pan: %1 (double-click to centre)", strip.panText(value))
            QQC2.ToolTip.visible: hovered || pressed
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            TapHandler {
                onDoubleTapped: Mixer.setPan(strip.index, 0)
            }
        }

        // Fader and meter
        RowLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.minimumHeight: Kirigami.Units.gridUnit * 6
            spacing: Kirigami.Units.smallSpacing

            QQC2.Slider {
                id: fader
                Layout.fillHeight: true
                Layout.alignment: Qt.AlignHCenter
                orientation: Qt.Vertical
                from: -60
                to: 6
                value: strip.volumeDb
                onMoved: Mixer.setVolume(strip.index, value)
                onPressedChanged: Mixer.touch(strip.index, "volume", -1, pressed)
                Accessible.name: i18nc("@label:slider", "Volume")
                QQC2.ToolTip.text: i18nc("@info:tooltip", "Volume (double-click for 0 dB)")
                QQC2.ToolTip.visible: hovered && !pressed
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                TapHandler {
                    onDoubleTapped: Mixer.setVolume(strip.index, 0)
                }
            }
            LevelMeter {
                Layout.fillHeight: true
                levels: [strip.levelLeft, strip.levelRight]
                holds: [strip.holdLeft, strip.holdRight]
            }
        }
        QQC2.Label {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            font.features: { "tnum": 1 }
            text: strip.dbText(fader.value)
        }

        // Mute and solo
        RowLayout {
            Layout.fillWidth: true
            opacity: strip.isMaster ? 0 : 1
            enabled: !strip.isMaster
            spacing: Kirigami.Units.smallSpacing
            QQC2.Button {
                Layout.fillWidth: true
                checkable: true
                checked: strip.muted
                text: i18nc("@action:button short for Mute", "M")
                Accessible.name: i18nc("@action:button", "Mute")
                QQC2.ToolTip.text: i18nc("@info:tooltip", "Mute")
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                onToggled: Mixer.setMuted(strip.index, checked)
            }
            QQC2.Button {
                Layout.fillWidth: true
                checkable: true
                checked: strip.soloed
                text: i18nc("@action:button short for Solo", "S")
                Accessible.name: i18nc("@action:button", "Solo")
                QQC2.ToolTip.text: i18nc("@info:tooltip", "Solo")
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                onToggled: Mixer.setSoloed(strip.index, checked)
            }
        }

        // Output: the master or a bus that doesn't feed this strip. The
        // master's own output is the hardware, so its space stays empty.
        QQC2.ComboBox {
            Layout.fillWidth: true
            opacity: strip.isMaster ? 0 : 1
            enabled: !strip.isMaster
            Accessible.ignored: strip.isMaster
            model: strip.outputChoices.map(name => name || strip.masterName)
            currentIndex: strip.outputIndex
            // A new model resets the index without re-running the binding,
            // so a renamed or added bus would show the master; bind again.
            onModelChanged: currentIndex = Qt.binding(() => strip.outputIndex)
            onActivated: index => Mixer.setOutput(strip.index, index)
            Accessible.name: i18nc("@label:listbox", "Output")
            QQC2.ToolTip.text: i18nc("@info:tooltip", "Output: %1", displayText)
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }
    }
}
