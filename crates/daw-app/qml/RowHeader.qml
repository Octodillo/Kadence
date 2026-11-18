// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// The header of one row under a track (the `rows` role of Tracks). A row
// is either the heading of a group (Instrument, Effects, one effect,
// Sends), which opens and closes it, or one control: its name, the control
// itself, the mode of its automation lane (off, read, write) and, once it
// has a lane, a button to remove that. The lane itself is drawn beside the
// header, in the timeline; clicking there starts one.
//
// Headings and sends also do what the track's channel strip does with
// them, through the Mixer: Effects adds an effect and Sends a send, an
// effect or the instrument shows its settings, and an effect or a send
// can be removed.
Rectangle {
    id: rowHeader

    property int track: -1
    // The track's strip among the Mixer's rows (the `strip` role of
    // Tracks), for the Mixer's actions on it.
    property int strip: -1
    // The row among the track's rows.
    property int index: -1
    // The row as one entry of the track's `rows` role (see Tracks).
    property var info: null
    // Whether the track has an instrument, whose settings its Instrument
    // heading shows.
    property bool hasInstrument: false

    // An Automation.LaneKind; -1 for no row.
    readonly property int kind: info ? info.kind : -1
    readonly property bool isControl: kind >= 0 && kind <= Automation.DeviceParamLane
    readonly property bool isGroup: kind > Automation.DeviceParamLane
    readonly property int depth: info ? info.depth : 0
    readonly property bool open: info ? info.open : false
    readonly property string name: info ? rowName.text(kind, info.detail, info.param) : ""
    // The control's lane among the track's lanes, -1 without one.
    readonly property int lane: info ? info.lane : -1
    // An Automation.LaneMode.
    readonly property int mode: info ? info.mode : Automation.LaneRead
    // An effect heading's insert, or a send level's send, on the strip;
    // -1 for other rows.
    readonly property int item: info ? info.item : -1
    readonly property bool isSend: kind === Automation.SendLane && item >= 0
    readonly property bool isEffect: kind === Automation.DeviceGroup && item >= 0
    // Whether the strip has a bus left to send to (a Sends heading only).
    property bool canSend: false

    // What a heading or a send does to the track's strip, through the
    // Mixer; the row's buttons and its menu share these.
    readonly property Kirigami.Action addAction: Kirigami.Action {
        readonly property bool effects: rowHeader.kind === Automation.EffectsGroup
        visible: effects || rowHeader.kind === Automation.SendsGroup
        enabled: effects || rowHeader.canSend
        icon.name: "list-add"
        text: effects ? i18nc("@action", "Add Effect") : i18nc("@action", "Add Send")
        onTriggered: (effects ? effectMenu : sendMenu).popup(addButton, 0, addButton.height)
    }
    readonly property Kirigami.Action settingsAction: Kirigami.Action {
        visible: rowHeader.isEffect || (rowHeader.kind === Automation.InstrumentGroup && rowHeader.hasInstrument)
        icon.name: "configure"
        text: rowHeader.isEffect
            ? i18nc("@action %1 is an effect's name", "Show the Settings of %1", rowHeader.name)
            : i18nc("@action", "Show Instrument Settings")
        onTriggered: DevicePanel.open(rowHeader.isEffect
            ? Mixer.insertDevice(rowHeader.strip, rowHeader.item)
            : Mixer.instrumentDevice(rowHeader.strip))
    }
    readonly property Kirigami.Action removeAction: Kirigami.Action {
        visible: rowHeader.isEffect || rowHeader.isSend
        icon.name: "edit-delete-remove"
        text: rowHeader.isEffect
            ? i18nc("@action %1 is an effect's name", "Remove %1", rowHeader.name)
            : i18nc("@action %1 is a bus", "Remove Send to %1", rowHeader.info?.detail ?? "")
        onTriggered: {
            if (rowHeader.isEffect) {
                Mixer.removeInsert(rowHeader.strip, rowHeader.item);
            } else {
                Mixer.removeSend(rowHeader.strip, rowHeader.item);
            }
            rowHeader.edited();
        }
    }

    // After an edit through the Mixer: the rows show it at once, not on
    // the next beat, as after an edit in the track header.
    function edited(): void {
        Project.tick();
    }

    function readStrip(): void {
        canSend = kind === Automation.SendsGroup && (Mixer.get(strip).sendChoices ?? []).length > 0;
    }
    onKindChanged: readStrip()
    onStripChanged: readStrip()
    // The Mixer follows the project while the arrange view shows (see
    // ArrangePage), so a bus that comes or goes shows here.
    Connections {
        target: Mixer
        enabled: rowHeader.kind === Automation.SendsGroup
        function onModelReset() { rowHeader.readStrip(); }
        function onDataChanged() { rowHeader.readStrip(); }
    }

    // The control as it is now: its value in its unit, and whether its
    // lane's points set it as it plays.
    property real value: 0
    property bool following: false
    // Moving the control during playback records over its lane.
    property bool writes: false
    // False for a parameter nothing describes (its plugin is not loaded):
    // there is no scale to move it along.
    property bool editable: true
    // How a device parameter's value is worded (see ValueText.param).
    readonly property string paramControl: info ? info.control : ""
    readonly property string paramUnit: info ? info.unit : ""
    readonly property var paramChoices: info ? info.choices : []

    function readValue(): void {
        if (!isControl) {
            return;
        }
        const control = Automation.rowControl(track, index);
        value = control.value;
        following = control.following;
        writes = control.writes;
        editable = control.editable;
        if (!slider.pressed) {
            slider.value = control.position;
        }
    }

    onInfoChanged: readValue()
    onTrackChanged: readValue()
    onIndexChanged: readValue()
    Component.onCompleted: readValue()

    // An edit or an undo can move the control without changing `info`.
    Connections {
        target: UiClock
        function onRevised() { rowHeader.readValue(); }
    }

    // A playing lane moves its control.
    Connections {
        target: UiClock
        enabled: rowHeader.following && Project.playing && rowHeader.visible
        function onTicked() { rowHeader.readValue(); }
    }
    // So does moving the playhead while stopped.
    Connections {
        target: Project
        enabled: rowHeader.following && !Project.playing
        function onPositionTextChanged() {
            rowHeader.readValue();
        }
        function onTimeTextChanged() {
            rowHeader.readValue();
        }
    }

    // The value with its unit, for the tooltip and screen readers.
    readonly property string valueText: {
        switch (kind) {
        case Automation.VolumeLane:
        case Automation.SendLane:
            return ValueText.level(value);
        case Automation.PanLane:
            return ValueText.pan(value);
        case Automation.DeviceParamLane:
            return ValueText.param(value, paramControl, paramUnit, paramChoices);
        }
        return "";
    }

    // Closes the row's menus, if open.
    function dismiss(): void {
        rowMenu.dismiss();
        effectMenu.dismiss();
        sendMenu.dismiss();
    }

    // Like the timeline's rows under a track: a shade off the background.
    color: Kirigami.ColorUtils.tintWithAlpha(Kirigami.Theme.backgroundColor, Kirigami.Theme.textColor, 0.04)

    RowName {
        id: rowName
    }

    RowLayout {
        anchors.fill: parent
        // Past the track's colour and expand button, and a step further
        // in for each group the row is inside.
        anchors.leftMargin: Kirigami.Units.gridUnit * (1 + rowHeader.depth)
        anchors.rightMargin: Kirigami.Units.smallSpacing
        spacing: Kirigami.Units.smallSpacing

        // A group's heading: opens and closes it. A plain button, drawn
        // here like a list item (arrow and name from the left); a
        // ToolButton would centre them.
        QQC2.AbstractButton {
            id: heading
            visible: rowHeader.isGroup
            Layout.fillWidth: true
            Layout.fillHeight: true
            text: rowHeader.name
            onClicked: Automation.setRowOpen(rowHeader.track, rowHeader.index, !rowHeader.open)
            Accessible.name: rowHeader.name
            Accessible.description: rowHeader.open
                ? i18nc("@info:whatsthis a group of controls", "Open. Closing it leaves only the automated controls in view.")
                : i18nc("@info:whatsthis a group of controls", "Closed. Only the automated controls show.")
            QQC2.ToolTip.text: rowHeader.open
                ? i18nc("@info:tooltip", "Close: keep only the automated controls in view")
                : i18nc("@info:tooltip", "Open: show every control")
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            background: Rectangle {
                color: Kirigami.Theme.hoverColor
                opacity: heading.down ? 0.3 : heading.hovered || heading.visualFocus ? 0.15 : 0
                radius: Kirigami.Units.cornerRadius
            }
            contentItem: RowLayout {
                spacing: Kirigami.Units.smallSpacing
                Kirigami.Icon {
                    Layout.preferredWidth: Kirigami.Units.iconSizes.small
                    Layout.preferredHeight: Kirigami.Units.iconSizes.small
                    Layout.alignment: Qt.AlignVCenter
                    source: rowHeader.open ? "arrow-down" : "arrow-right"
                }
                QQC2.Label {
                    Layout.fillWidth: true
                    Layout.alignment: Qt.AlignVCenter
                    text: rowHeader.name
                    elide: Text.ElideRight
                    font: Kirigami.Theme.smallFont
                }
            }
        }

        // A heading's actions, as buttons. (A send's are in its menu: the
        // control's row has no room for them.)
        QQC2.ToolButton {
            id: addButton
            action: rowHeader.addAction
            visible: action.visible
            Layout.alignment: Qt.AlignVCenter
            display: QQC2.AbstractButton.IconOnly
            QQC2.ToolTip.text: text
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }
        Repeater {
            model: [rowHeader.settingsAction, rowHeader.removeAction]
            delegate: QQC2.ToolButton {
                required property Kirigami.Action modelData
                action: modelData
                visible: modelData.visible && rowHeader.isGroup
                Layout.alignment: Qt.AlignVCenter
                display: QQC2.AbstractButton.IconOnly
                QQC2.ToolTip.text: text
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            }
        }

        QQC2.Label {
            visible: rowHeader.isControl
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignVCenter
            text: rowHeader.name
            elide: Text.ElideRight
            font: Kirigami.Theme.smallFont
            opacity: rowHeader.mode === Automation.LaneOff ? 0.6 : 1
        }

        // The control itself, like the fader, pan knob or parameter
        // slider elsewhere. It follows its lane while the lane plays.
        QQC2.Slider {
            id: slider
            visible: rowHeader.isControl
            Layout.preferredWidth: Kirigami.Units.gridUnit * 5
            Layout.alignment: Qt.AlignVCenter
            from: 0
            to: 1
            enabled: rowHeader.editable && (!rowHeader.following || rowHeader.writes)
            // A press and its release are one gesture, and one undo step;
            // a key press is a gesture of its own.
            function move(): void {
                Automation.setRowPosition(rowHeader.track, rowHeader.index, value);
                rowHeader.value = Automation.rowControl(rowHeader.track, rowHeader.index).value;
            }
            onMoved: {
                if (pressed) {
                    move();
                    return;
                }
                Automation.beginRowControl(rowHeader.track, rowHeader.index);
                move();
                Automation.endRowControl(rowHeader.track, rowHeader.index);
            }
            onPressedChanged: {
                if (pressed) {
                    Automation.beginRowControl(rowHeader.track, rowHeader.index);
                } else {
                    Automation.endRowControl(rowHeader.track, rowHeader.index);
                }
            }
            Accessible.name: rowHeader.name
            Accessible.description: rowHeader.valueText
            // Disabled controls get no hover, so only while it can move.
            QQC2.ToolTip.text: rowHeader.valueText
            QQC2.ToolTip.visible: hovered || pressed
            QQC2.ToolTip.delay: pressed ? 0 : Kirigami.Units.toolTipDelay
        }

        QQC2.ComboBox {
            visible: rowHeader.isControl
            Layout.alignment: Qt.AlignVCenter
            Layout.maximumWidth: Kirigami.Units.gridUnit * 4.5
            flat: true
            font: Kirigami.Theme.smallFont
            // The modes in the order the list shows them.
            readonly property list<int> modes: [Automation.LaneOff, Automation.LaneRead, Automation.LaneWrite]
            model: [
                i18nc("@item:inlistbox automation lane mode", "Off"),
                i18nc("@item:inlistbox automation lane mode", "Read"),
                i18nc("@item:inlistbox automation lane mode", "Write")
            ]
            currentIndex: modes.indexOf(rowHeader.mode)
            onActivated: (i) => {
                Automation.setRowMode(rowHeader.track, rowHeader.index, modes[i]);
                // Choosing Off without a lane changes nothing: show that.
                currentIndex = Qt.binding(() => modes.indexOf(rowHeader.mode));
            }
            Accessible.name: i18nc("@label:listbox", "Automation mode")
            QQC2.ToolTip.text: i18nc("@info:tooltip", "Off: the control keeps its own setting. Read: its automation plays. Write: it plays, and moving the control while playing records over it.")
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }

        // Keeps its place without a lane, so the controls stay in line.
        QQC2.ToolButton {
            visible: rowHeader.isControl
            enabled: rowHeader.lane >= 0
            opacity: enabled ? 1 : 0
            Layout.alignment: Qt.AlignVCenter
            icon.name: "list-remove"
            display: QQC2.AbstractButton.IconOnly
            text: i18nc("@action:button", "Remove Automation")
            onClicked: Automation.removeLane(rowHeader.track, rowHeader.lane)
            Accessible.ignored: !enabled
            QQC2.ToolTip.text: text
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }
    }

    Kirigami.Separator {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        opacity: 0.5
    }

    LazyMenu {
        id: effectMenu
        menu: AddEffectMenu {
            row: rowHeader.strip
            onClosed: rowHeader.edited()
        }
    }
    LazyMenu {
        id: sendMenu
        menu: AddSendMenu {
            row: rowHeader.strip
            choices: Mixer.get(rowHeader.strip).sendChoices ?? []
            onClosed: rowHeader.edited()
        }
    }

    LazyMenu {
        id: rowMenu
        menu: QQC2.Menu {
            id: contextMenu
            // The strip as the menu opened.
            readonly property var stripInfo: Mixer.get(rowHeader.strip)
            QQC2.MenuItem {
                visible: rowHeader.isGroup
                height: visible ? implicitHeight : 0
                text: rowHeader.open ? i18nc("@action:inmenu a group of controls", "Close") : i18nc("@action:inmenu a group of controls", "Open")
                icon.name: rowHeader.open ? "arrow-right" : "arrow-down"
                onTriggered: Automation.setRowOpen(rowHeader.track, rowHeader.index, !rowHeader.open)
            }
            QQC2.MenuItem {
                visible: rowHeader.isControl
                height: visible ? implicitHeight : 0
                enabled: rowHeader.lane >= 0
                text: i18nc("@action:inmenu", "Remove Automation")
                icon.name: "list-remove"
                onTriggered: Automation.removeLane(rowHeader.track, rowHeader.lane)
            }
            Repeater {
                model: [rowHeader.addAction, rowHeader.settingsAction, rowHeader.removeAction]
                delegate: QQC2.MenuItem {
                    required property Kirigami.Action modelData
                    action: modelData
                    visible: modelData.visible
                    height: visible ? implicitHeight : 0
                }
            }
            // A send taps the track before or after its fader.
            QQC2.MenuItem {
                visible: rowHeader.isSend
                height: visible ? implicitHeight : 0
                checkable: true
                checked: (contextMenu.stripInfo.sendPreFader ?? [])[rowHeader.item] === true
                text: i18nc("@action:inmenu a send taken before the fader", "Pre-Fader")
                onTriggered: {
                    Mixer.setSendPreFader(rowHeader.strip, rowHeader.item, checked);
                    rowHeader.edited();
                }
            }
            QQC2.MenuItem {
                text: i18nc("@action:inmenu", "Hide Controls")
                icon.name: "arrow-right"
                onTriggered: Automation.setExpanded(rowHeader.track, false)
            }
        }
    }

    TapHandler {
        acceptedButtons: Qt.RightButton
        onTapped: rowMenu.popup()
    }
    // Touch screens have no right button: hold a finger instead.
    TapHandler {
        acceptedDevices: PointerDevice.TouchScreen
        // Under the finger: a touch doesn't move the mouse cursor.
        onLongPressed: rowMenu.popup(parent, point.position.x, point.position.y)
    }
}
