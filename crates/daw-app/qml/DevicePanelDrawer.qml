// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// The open device's controls, in a drawer on the right: bypass, its place
// in the chain, and a control per parameter. Open it with
// DevicePanel.open(id) from anywhere; it closes when the device goes away.
Kirigami.OverlayDrawer {
    id: drawer

    edge: Qt.application.layoutDirection === Qt.RightToLeft ? Qt.LeftEdge : Qt.RightEdge
    // Beside the page on a wide window, so the mixer stays usable; over it
    // on a narrow one.
    modal: !applicationWindow().wideScreen
    handleVisible: false
    width: Math.min(Kirigami.Units.gridUnit * 22, applicationWindow().width * 0.8)
    leftPadding: 0
    rightPadding: 0
    topPadding: 0
    bottomPadding: 0

    readonly property string masterName: i18nc("@title name of the master bus", "Master")

    // Kirigami closes the drawer when it turns modal or back (the window
    // was resized); open it again rather than dropping the device.
    property bool reopening: false
    onModalChanged: {
        if (DevicePanel.active) {
            reopening = true;
            Qt.callLater(() => {
                reopening = false;
                if (DevicePanel.active) {
                    drawer.open();
                }
            });
        }
    }

    onAboutToShow: DevicePanel.refresh()
    onClosed: {
        if (!reopening) {
            DevicePanel.close();
        }
    }

    DeviceNames {
        id: deviceNames
    }

    Connections {
        target: DevicePanel
        function onActiveChanged(): void {
            if (DevicePanel.active) {
                drawer.open();
            } else {
                drawer.close();
            }
        }
    }

    // Edits made elsewhere (undo, the mixer, a removed device).
    Timer {
        interval: 100
        repeat: true
        running: drawer.visible
        onTriggered: {
            // Before the panel, so a window the user just closed shows
            // the values they set there.
            PluginEditors.refresh();
            DevicePanel.refresh();
        }
    }

    // A parameter value with its unit, in the user's language.
    function valueText(value: real, control: string, unit: string, choices: var): string {
        const num = (v, decimals) => v.toLocaleString(Qt.locale(), "f", decimals);
        const signed = (v, decimals) => (v > 0 ? "+" : "") + num(v, decimals);
        switch (control) {
        case "toggle":
            return value >= 0.5 ? i18nc("@label parameter switched on", "On")
                                : i18nc("@label parameter switched off", "Off");
        case "choice":
            return deviceNames.name(choices[Math.round(value)] ?? "");
        case "integer":
            return num(Math.round(value), 0);
        }
        switch (unit) {
        case "dB":
            // daw_devices::dsp::MIN_DB and below is silence.
            return value <= -70 ? i18nc("@label silence in decibels", "−∞ dB")
                                 : i18nc("@label decibels", "%1 dB", num(value, 1));
        case "Hz":
            return value >= 1000 ? i18nc("@label kilohertz", "%1 kHz", num(value / 1000, 2))
                                 : i18nc("@label hertz", "%1 Hz", num(value, 0));
        case "ms":
            return value >= 1000 ? i18nc("@label seconds", "%1 s", num(value / 1000, 2))
                                 : i18nc("@label milliseconds", "%1 ms", num(value, 1));
        case "percent":
            return i18nc("@label percent", "%1 %", num(value * 100, 0));
        case "semitones":
            return i18nc("@label semitones", "%1 st", signed(Math.round(value), 0));
        case "cents":
            return i18nc("@label cents", "%1 ct", signed(Math.round(value), 0));
        case "octaves":
            return i18nc("@label octaves", "%1 oct", signed(value, 2));
        case "ratio":
            return i18nc("@label compression ratio", "%1:1", num(value, 1));
        case "pan": {
            const pct = Math.round(Math.abs(value) * 100);
            if (pct === 0) {
                return i18nc("@label pan centre", "C");
            }
            return value < 0 ? i18nc("@label pan left, percent", "L%1", pct)
                             : i18nc("@label pan right, percent", "R%1", pct);
        }
        }
        return num(value, 2);
    }

    contentItem: ColumnLayout {
        spacing: 0

        // Title, bypass and chain position.
        QQC2.ToolBar {
            Layout.fillWidth: true
            position: QQC2.ToolBar.Header
            leftPadding: Kirigami.Units.largeSpacing
            rightPadding: Kirigami.Units.smallSpacing
            bottomPadding: Kirigami.Units.smallSpacing

            contentItem: ColumnLayout {
                spacing: 0
                RowLayout {
                    Layout.fillWidth: true
                    Kirigami.Heading {
                        Layout.fillWidth: true
                        level: 2
                        text: deviceNames.name(DevicePanel.deviceName)
                        elide: Text.ElideRight
                    }
                    QQC2.ToolButton {
                        icon.name: "dialog-close"
                        display: QQC2.AbstractButton.IconOnly
                        text: i18nc("@action:button", "Close")
                        QQC2.ToolTip.text: text
                        QQC2.ToolTip.visible: hovered
                        QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                        onClicked: drawer.close()
                    }
                }
                QQC2.Label {
                    Layout.fillWidth: true
                    // The master is unnamed unless the user renamed it.
                    text: i18nc("@label %1 is a track name", "On %1",
                                DevicePanel.trackName || (DevicePanel.onMaster ? drawer.masterName : ""))
                    color: Kirigami.Theme.disabledTextColor
                    elide: Text.ElideRight
                    visible: text !== ""
                }
                RowLayout {
                    Layout.fillWidth: true
                    Layout.topMargin: Kirigami.Units.smallSpacing
                    QQC2.Switch {
                        id: onSwitch
                        Layout.fillWidth: true
                        text: i18nc("@option:check device processes audio, not bypassed", "Active")
                        checked: DevicePanel.switchedOn
                        onToggled: DevicePanel.setDeviceEnabled(checked)
                        QQC2.ToolTip.text: DevicePanel.instrument
                                           ? i18nc("@info:tooltip", "Switch off to silence the instrument")
                                           : i18nc("@info:tooltip", "Switch off to bypass the effect")
                        QQC2.ToolTip.visible: hovered
                        QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                    }
                    QQC2.ToolButton {
                        icon.name: Qt.application.layoutDirection === Qt.RightToLeft ? "go-next" : "go-previous"
                        display: QQC2.AbstractButton.IconOnly
                        text: i18nc("@action:button move the device earlier in the chain", "Move Earlier")
                        enabled: DevicePanel.canMoveEarlier
                        QQC2.ToolTip.text: text
                        QQC2.ToolTip.visible: hovered
                        QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                        onClicked: DevicePanel.moveBy(-1)
                    }
                    QQC2.ToolButton {
                        icon.name: Qt.application.layoutDirection === Qt.RightToLeft ? "go-previous" : "go-next"
                        display: QQC2.AbstractButton.IconOnly
                        text: i18nc("@action:button move the device later in the chain", "Move Later")
                        enabled: DevicePanel.canMoveLater
                        QQC2.ToolTip.text: text
                        QQC2.ToolTip.visible: hovered
                        QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                        onClicked: DevicePanel.moveBy(1)
                    }
                    // The plugin's own window, for plugins that have one.
                    QQC2.ToolButton {
                        objectName: "showPluginInterface"
                        icon.name: "preferences-plugin"
                        display: QQC2.AbstractButton.IconOnly
                        text: checked
                              ? i18nc("@action:button close the plugin's own window", "Hide Plugin Interface")
                              : i18nc("@action:button open the plugin's own window", "Show Plugin Interface")
                        // `revision` in the expression: a bare statement is
                        // compiled away, and the binding would not re-read.
                        visible: PluginEditors.revision >= 0 && DevicePanel.active
                                 && PluginEditors.hasEditor(DevicePanel.deviceId)
                        checkable: true
                        checked: PluginEditors.revision >= 0 && PluginEditors.isOpen(DevicePanel.deviceId)
                        QQC2.ToolTip.text: text
                        QQC2.ToolTip.visible: hovered
                        QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                        onToggled: {
                            if (checked) {
                                PluginEditors.show(DevicePanel.deviceId);
                            } else {
                                PluginEditors.close(DevicePanel.deviceId);
                            }
                            // Back to what the plugin says, whatever happened.
                            checked = Qt.binding(() => PluginEditors.revision >= 0
                                                       && PluginEditors.isOpen(DevicePanel.deviceId));
                        }
                    }
                }
            }
        }

        // A plugin that doesn't play, and why; a crashed one can restart.
        Kirigami.InlineMessage {
            id: pluginMessage
            objectName: "pluginMessage"
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            readonly property string status: DevicePanel.pluginStatus
            visible: status !== "" && status !== "running"
            type: status === "crashed" || status === "failed" ? Kirigami.MessageType.Error
                                                               : Kirigami.MessageType.Warning
            text: {
                switch (status) {
                case "crashed":
                    return i18nc("@info", "The plugin stopped working and is bypassed.");
                case "missing":
                    return i18nc("@info", "The plugin isn't installed on this computer, so it is bypassed.");
                case "failed":
                    return DevicePanel.pluginError
                           ? i18nc("@info %1 is the plugin's own error message", "The plugin couldn't be loaded, so it is bypassed: %1", DevicePanel.pluginError)
                           : i18nc("@info", "The plugin couldn't be loaded, so it is bypassed.");
                case "unavailable":
                    return i18nc("@info", "Plugins can't be loaded right now, so this one is bypassed.");
                }
                return "";
            }
            actions: [
                Kirigami.Action {
                    icon.name: "view-refresh"
                    text: i18nc("@action:button restart a crashed plugin", "Reload Plugin")
                    visible: pluginMessage.status === "crashed"
                    onTriggered: DevicePanel.reloadPlugin()
                }
            ]
        }

        QQC2.ScrollView {
            id: scroll
            Layout.fillWidth: true
            Layout.fillHeight: true
            QQC2.ScrollBar.horizontal.policy: QQC2.ScrollBar.AlwaysOff
            contentWidth: availableWidth

            ColumnLayout {
                width: scroll.availableWidth
                spacing: 0

                Repeater {
                    model: DevicePanel
                    delegate: ParamRow {}
                }
            }
        }

        Kirigami.PlaceholderMessage {
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.margins: Kirigami.Units.largeSpacing
            visible: DevicePanel.active && DevicePanel.paramCount === 0
            icon.name: "configure"
            text: i18nc("@info:placeholder", "No settings to show")
            explanation: i18nc("@info:placeholder", "This device has no parameters Kadence can show.")
        }
    }

    // One parameter: its name and value, and the control for its kind.
    component ParamRow: ColumnLayout {
        id: row

        required property int index
        required property string name
        required property double value
        required property double position
        required property double defaultValue
        required property double minimum
        required property double maximum
        required property string control
        required property string unit
        required property list<string> choices
        required property bool isDefault
        required property bool automated
        required property bool writable
        readonly property string label: deviceNames.name(name)

        readonly property string valueText: drawer.valueText(value, control, unit, choices)
        readonly property string defaultText: drawer.valueText(defaultValue, control, unit, choices)

        Layout.fillWidth: true
        Layout.leftMargin: Kirigami.Units.largeSpacing
        Layout.rightMargin: Kirigami.Units.largeSpacing
        Layout.topMargin: Kirigami.Units.smallSpacing
        Layout.bottomMargin: Kirigami.Units.smallSpacing
        spacing: 0

        RowLayout {
            Layout.fillWidth: true
            visible: row.control !== "toggle"
            QQC2.Label {
                Layout.fillWidth: true
                text: row.label
                elide: Text.ElideRight
            }
            Kirigami.Icon {
                Layout.preferredWidth: Kirigami.Units.iconSizes.small
                Layout.preferredHeight: Kirigami.Units.iconSizes.small
                visible: row.automated
                source: "media-record-symbolic"
                HoverHandler { id: automatedHover }
                QQC2.ToolTip.text: i18nc("@info:tooltip", "Automated: an automation lane sets this while playing")
                QQC2.ToolTip.visible: automatedHover.hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                Accessible.name: i18nc("@info", "Automated")
            }
            QQC2.Label {
                text: row.control === "continuous" && slider.pressed
                      ? drawer.valueText(DevicePanel.valueAt(row.index, slider.value), row.control, row.unit, row.choices)
                      : row.valueText
                font.features: { "tnum": 1 }
                color: row.isDefault ? Kirigami.Theme.textColor : Kirigami.Theme.highlightColor
            }
        }

        QQC2.Slider {
            id: slider
            Layout.fillWidth: true
            visible: row.control === "continuous"
            // The lane overrides an edit here, so don't offer one, unless
            // a drag during playback writes the lane.
            enabled: !row.automated || row.writable
            from: 0
            to: 1
            value: row.position
            onMoved: DevicePanel.setPosition(row.index, value, pressed)
            onPressedChanged: {
                if (!pressed) {
                    DevicePanel.endGesture();
                }
            }
            Accessible.name: row.label
            QQC2.ToolTip.text: i18nc("@info:tooltip %1 is a parameter's default value",
                                     "Double-click to reset to %1", row.defaultText)
            QQC2.ToolTip.visible: hovered && !pressed
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            TapHandler {
                onDoubleTapped: DevicePanel.reset(row.index)
            }
        }

        QQC2.SpinBox {
            Layout.fillWidth: true
            visible: row.control === "integer"
            // The lane overrides an edit here, so don't offer one.
            enabled: !row.automated
            from: Math.round(row.minimum)
            to: Math.round(row.maximum)
            value: Math.round(row.value)
            editable: true
            onValueModified: DevicePanel.setValue(row.index, value, false)
            Accessible.name: row.label
        }

        QQC2.ComboBox {
            Layout.fillWidth: true
            visible: row.control === "choice"
            // The lane overrides an edit here, so don't offer one.
            enabled: !row.automated
            model: row.choices.map(c => deviceNames.name(c))
            currentIndex: Math.round(row.value)
            onActivated: index => DevicePanel.setValue(row.index, index, false)
            Accessible.name: row.label
        }

        QQC2.Switch {
            Layout.fillWidth: true
            visible: row.control === "toggle"
            // The lane overrides an edit here, so don't offer one.
            enabled: !row.automated
            text: row.label
            checked: row.value >= 0.5
            onToggled: DevicePanel.setValue(row.index, checked ? 1 : 0, false)
        }
    }
}
