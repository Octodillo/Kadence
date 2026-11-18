// SPDX-License-Identifier: GPL-3.0-or-later

import QtQml
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// One row of the track list beside the timeline: colour, drag handle,
// name, instrument (on instrument tracks), MIDI input, record arm, audio
// input, mute, solo, and the button that shows the track's controls: the
// rows under it (volume, pan and the groups the rest fold into), whose
// headers follow under this one. It is a delegate of the Tracks
// model: what it shows are the model's roles, and its edits go to Tracks by
// row. Dragging the handle reorders tracks; the list view shows where the
// track will land (`dragFrom`, `dragTo`).
Column {
    id: row

    // The roles of Tracks.
    required property int index
    required property string trackId
    required property bool selected
    required property string name
    required property int kind
    required property color color
    required property bool muted
    required property bool soloed
    required property bool armed
    required property int input
    required property bool inputStereo
    required property bool monitorAuto
    required property string midiDevice
    required property int midiChannel
    required property string instrument
    required property int instrumentChoice
    required property int strip
    required property bool expanded
    // The rows under the track while it is expanded (see Tracks).
    required property var rows

    // The timeline, for row geometry.
    required property ArrangeView arrange
    // The automation lanes' height (see ArrangePage).
    required property LayoutSize laneSize
    // The page's actions on the selected track, for the header's menu:
    // rename, move up, move down, delete.
    required property list<Kirigami.Action> trackActions

    // How much each of `r` sizes in this row (the track, or the lanes down
    // to a dragged edge) changes so the edge moves `delta` with the pointer.
    // Normally the row's top stays put by scrolling past the `above` sizes
    // above it, which change too. When the view can't scroll that far (the
    // content, `total` such sizes in all, is `content` tall and fits in
    // `visible`, or the view would scroll above the top), the scroll stays
    // at the top, and all `above + r` sizes share the movement.
    function edgeChange(delta: real, scroll: real, above: int, r: int, total: int, content: real, visible: real): real {
        const change = delta / r;
        const to = scroll + above * change;
        const most = Math.max(0, content + total * change - visible);
        if (to < 0 || (to > most && most === 0)) {
            return (delta - scroll) / (above + r);
        }
        return change;
    }

    // The view's height below the ruler, which rows scroll in.
    function visibleHeight(): real {
        return arrange.height - arrange.rulerHeight;
    }

    // How many lane rows lie above this row, as laid out now.
    function lanesAbove(): int {
        return arrange.laneHeight > 0 ? Math.round((y - index * arrange.trackHeight) / arrange.laneHeight) : 0;
    }

    // A header is a row, and its track can change under it: the list reuses
    // headers that scroll out of view, and adding, removing or moving a
    // track shifts the rows. An open name field or menu would then rename,
    // move or delete the track that took the row, so close them.
    ListView.onPooled: dismiss()
    onTrackIdChanged: dismiss()

    readonly property bool renaming: nameField.visible
    // Opens the name for editing; again while it is open changes nothing.
    function rename(): void {
        if (renaming) {
            return;
        }
        nameField.text = row.name;
        nameField.visible = true;
        nameField.forceActiveFocus();
        nameField.selectAll();
    }

    // The header of row `i` under the track, or null.
    function rowHeader(i: int): RowHeader {
        return rowHeaders.itemAt(i) as RowHeader;
    }

    function dismiss(): void {
        nameField.focus = false;
        nameField.visible = false;
        menu.dismiss();
        inputMenu.dismiss();
        instrumentButton.dismiss();
        midiInputButton.dismiss();
        for (let i = 0; i < rowHeaders.count; ++i) {
            rowHeaders.itemAt(i).dismiss();
        }
    }

    QQC2.ItemDelegate {
        id: header

        readonly property int index: row.index
        width: row.width
        height: row.arrange.trackHeight

        // Audio and instrument tracks record.
        readonly property bool canRecord: row.kind === Tracks.AudioTrack || row.kind === Tracks.InstrumentTrack
        // Inputs are counted from 1 for people.
        readonly property string inputText: row.inputStereo
            ? i18nc("@item a pair of audio inputs, %1 and %2 are input numbers", "Inputs %1–%2", row.input + 1, row.input + 2)
            : i18nc("@item one audio input, %1 is its number", "Input %1", row.input + 1)

        highlighted: row.selected
        opacity: row.ListView.view && row.ListView.view.dragFrom === index ? 0.5 : 1
        padding: 0
        leftPadding: 0
        onClicked: Project.selectTrack(index)

        Accessible.name: row.name

        contentItem: RowLayout {
            spacing: Kirigami.Units.smallSpacing

            // The track colour, as in the clips.
            Rectangle {
                Layout.fillHeight: true
                Layout.preferredWidth: Kirigami.Units.smallSpacing
                color: row.color
            }

            QQC2.ToolButton {
                Layout.alignment: Qt.AlignVCenter
                icon.name: row.expanded ? "arrow-down" : "arrow-right"
                display: QQC2.AbstractButton.IconOnly
                text: row.expanded ? i18nc("@action:button", "Hide Controls") : i18nc("@action:button", "Show Controls")
                onClicked: Automation.setExpanded(header.index, !row.expanded)
                QQC2.ToolTip.text: text
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            }

            // Drag to reorder.
            Kirigami.Icon {
                id: handle
                Layout.preferredWidth: Kirigami.Units.iconSizes.small
                Layout.preferredHeight: Kirigami.Units.iconSizes.small
                Layout.alignment: Qt.AlignVCenter
                source: "handle-sort"
                isMask: true
                color: Kirigami.Theme.textColor
                opacity: handleHover.hovered || drag.active ? 1 : 0.5
                Accessible.name: i18nc("@info:accessible", "Drag to move the track")

                HoverHandler {
                    id: handleHover
                    cursorShape: drag.active ? Qt.ClosedHandCursor : Qt.OpenHandCursor
                }

                DragHandler {
                    id: drag
                    target: null
                    xAxis.enabled: false
                    // Keep the drag from the list's touch scrolling, which
                    // would otherwise take it over once the finger moves.
                    grabPermissions: PointerHandler.CanTakeOverFromAnything | PointerHandler.ApprovesTakeOverByNone
                    // The row the dragged track's middle is over.
                    readonly property int to: {
                        const mid = row.y + header.height / 2 + activeTranslation.y;
                        return row.arrange.rowAtContent(mid);
                    }
                    onActiveChanged: {
                        const list = row.ListView.view;
                        if (active) {
                            Project.selectTrack(header.index);
                            list.dragFrom = header.index;
                            list.dragTo = header.index;
                        } else {
                            const from = list.dragFrom;
                            const to = list.dragTo;
                            list.dragFrom = -1;
                            list.dragTo = -1;
                            if (from >= 0 && to >= 0 && from !== to) {
                                Tracks.moveTrack(from, to);
                            }
                        }
                    }
                    onToChanged: if (active) row.ListView.view.dragTo = to
                }
            }

            Kirigami.Icon {
                Layout.preferredWidth: Kirigami.Units.iconSizes.small
                Layout.preferredHeight: Kirigami.Units.iconSizes.small
                Layout.alignment: Qt.AlignVCenter
                source: row.kind === Tracks.InstrumentTrack ? "audio-midi" : row.kind === Tracks.BusTrack ? "view-media-equalizer" : "audio-x-generic"
                isMask: true
                color: Kirigami.Theme.textColor
            }

            QQC2.Label {
                id: nameLabel
                Layout.fillWidth: true
                Layout.alignment: Qt.AlignVCenter
                text: row.name
                elide: Text.ElideRight
                visible: !nameField.visible

                TapHandler {
                    onDoubleTapped: row.rename()
                }
            }

            QQC2.TextField {
                id: nameField
                Layout.fillWidth: true
                Layout.alignment: Qt.AlignVCenter
                visible: false
                onAccepted: {
                    Tracks.rename(header.index, text);
                    visible = false;
                }
                onActiveFocusChanged: if (!activeFocus) visible = false
                Keys.onEscapePressed: visible = false
            }

            InstrumentButton {
                id: instrumentButton
                Layout.alignment: Qt.AlignVCenter
                Layout.maximumWidth: Kirigami.Units.gridUnit * 5
                visible: row.kind === Tracks.InstrumentTrack
                instrument: row.instrument
                currentChoice: row.instrumentChoice
                onChosen: (choice, revision) => Tracks.setInstrument(header.index, choice, revision)
                onSettingsRequested: DevicePanel.open(Tracks.instrumentDevice(header.index))
            }

            MidiInputButton {
                id: midiInputButton
                Layout.alignment: Qt.AlignVCenter
                visible: row.kind === Tracks.InstrumentTrack
                device: row.midiDevice
                channel: row.midiChannel
                onChosen: (device, channel) => Tracks.setMidiInput(header.index, device, channel)
            }

            // Audio and instrument tracks record. Breeze's record icon is
            // always red, so draw the light: a ring when off, filled red when
            // armed.
            QQC2.ToolButton {
                id: armButton
                Layout.alignment: Qt.AlignVCenter
                visible: header.canRecord
                contentItem: Item {
                    implicitWidth: Kirigami.Units.iconSizes.smallMedium
                    implicitHeight: Kirigami.Units.iconSizes.smallMedium
                    Rectangle {
                        anchors.centerIn: parent
                        width: Math.round(parent.width * 0.6)
                        height: width
                        radius: width / 2
                        color: armButton.checked ? Kirigami.Theme.negativeTextColor : "transparent"
                        border.width: 2
                        border.color: armButton.checked ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.textColor
                    }
                }
                checkable: true
                checked: row.armed
                display: QQC2.AbstractButton.IconOnly
                text: i18nc("@action:button arm track for recording", "Arm")
                onToggled: Tracks.setArmed(header.index, checked)
                QQC2.ToolTip.text: i18nc("@info:tooltip", "Arm for recording. An armed instrument track plays what you play live.")
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            }

            // The audio input: which inputs to record and whether to monitor.
            QQC2.ToolButton {
                id: inputButton
                Layout.alignment: Qt.AlignVCenter
                visible: row.kind === Tracks.AudioTrack
                icon.name: "audio-input-microphone"
                display: QQC2.AbstractButton.IconOnly
                text: i18nc("@action:button %1 is the chosen input, like Input 1", "Audio input: %1", header.inputText)
                down: inputMenu.opened
                onClicked: inputMenu.popup(inputButton, 0, inputButton.height)
                QQC2.ToolTip.text: text
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay

                LazyMenu {
                    id: inputMenu
                    menu: QQC2.Menu {
                        id: inputPopup

                        // Each input on its own, then the pairs 1–2, 3–4 and so on,
                        // in one model so they stay in order above the separator.
                        Instantiator {
                            model: {
                                const choices = [];
                                for (let i = 0; i < Tracks.inputCount; ++i) {
                                    choices.push({ first: i, stereo: false });
                                }
                                for (let i = 0; i + 1 < Tracks.inputCount; i += 2) {
                                    choices.push({ first: i, stereo: true });
                                }
                                return choices;
                            }
                            delegate: QQC2.MenuItem {
                                required property var modelData
                                text: modelData.stereo
                                    ? i18nc("@item:inmenu a pair of audio inputs recorded as stereo, %1 and %2 are input numbers",
                                            "Inputs %1–%2 (Stereo)", modelData.first + 1, modelData.first + 2)
                                    : i18nc("@item:inmenu one audio input, %1 is its number", "Input %1", modelData.first + 1)
                                checkable: true
                                checked: row.inputStereo === modelData.stereo && row.input === modelData.first
                                // Re-picking the current choice keeps it checked.
                                onTriggered: {
                                    Tracks.setInput(header.index, modelData.first, modelData.stereo);
                                    checked = Qt.binding(() => row.inputStereo === modelData.stereo
                                                               && row.input === modelData.first);
                                }
                            }
                            onObjectAdded: (i, item) => inputPopup.insertItem(i, item)
                            onObjectRemoved: (i, item) => inputPopup.removeItem(item)
                        }

                        QQC2.MenuSeparator {}

                        QQC2.MenuItem {
                            text: i18nc("@option:check", "Monitor Input While Armed")
                            checkable: true
                            checked: row.monitorAuto
                            onTriggered: Tracks.setMonitorAuto(header.index, checked)
                        }
                    }
                }
            }

            QQC2.ToolButton {
                Layout.alignment: Qt.AlignVCenter
                icon.name: "audio-volume-muted"
                checkable: true
                checked: row.muted
                display: QQC2.AbstractButton.IconOnly
                text: i18nc("@action:button", "Mute")
                onToggled: Tracks.setMuted(header.index, checked)
                QQC2.ToolTip.text: text
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            }

            QQC2.ToolButton {
                Layout.alignment: Qt.AlignVCenter
                Layout.rightMargin: Kirigami.Units.smallSpacing
                icon.name: "audio-headphones"
                checkable: true
                checked: row.soloed
                display: QQC2.AbstractButton.IconOnly
                text: i18nc("@action:button", "Solo")
                onToggled: Tracks.setSoloed(header.index, checked)
                QQC2.ToolTip.text: text
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            }
        }

        // A line between rows, matching the timeline lanes.
        Kirigami.Separator {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
        }

        LazyMenu {
            id: menu
            menu: QQC2.Menu {
                // The header's track is the selected one whenever this
                // menu opens, so these are the page's actions on it.
                QQC2.MenuItem { action: row.trackActions[0] }
                QQC2.MenuItem { action: row.trackActions[1] }
                QQC2.MenuItem { action: row.trackActions[2] }
                QQC2.MenuSeparator {}
                QQC2.MenuItem {
                    text: row.expanded ? i18nc("@action:inmenu", "Hide Controls") : i18nc("@action:inmenu", "Show Controls")
                    icon.name: row.expanded ? "arrow-down" : "arrow-right"
                    onTriggered: Automation.setExpanded(header.index, !row.expanded)
                }
                // Where the track's sound goes: the master or a bus, as
                // the strip's output list on the Mix page.
                QQC2.Menu {
                    id: outputMenu
                    title: i18nc("@title:menu where the track's sound goes", "Output")
                    // The strip as the menu opened.
                    readonly property var stripInfo: Mixer.get(row.strip)
                    Repeater {
                        model: outputMenu.stripInfo.outputChoices ?? []
                        delegate: QQC2.MenuItem {
                            required property int index
                            required property string modelData
                            text: modelData || i18nc("@title name of the master bus", "Master")
                            checkable: true
                            checked: index === outputMenu.stripInfo.outputIndex
                            onTriggered: {
                                Mixer.setOutput(row.strip, index);
                                Project.tick();
                            }
                        }
                    }
                }
                QQC2.MenuSeparator {}
                QQC2.MenuItem { action: row.trackActions[3] }
            }
        }

        TapHandler {
            acceptedButtons: Qt.RightButton
            onTapped: {
                Project.selectTrack(header.index);
                menu.popup();
            }
        }
        // The keyboard's way to the same menu, under the header.
        Keys.forwardTo: [menuKey]
        MenuKey {
            id: menuKey
            onRequested: {
                Project.selectTrack(header.index);
                menu.popup(header, Kirigami.Units.gridUnit, header.height);
            }
        }
        // Touch screens have no right button: hold a finger instead.
        TapHandler {
            acceptedDevices: PointerDevice.TouchScreen
            onLongPressed: {
                Project.selectTrack(header.index);
                // Under the finger: a touch doesn't move the mouse cursor.
                menu.popup(parent, point.position.x, point.position.y);
            }
        }

        // Drag the lower edge to make every track taller or shorter (the
        // timeline has one track height), double-click for the default.
        // This row's top stays put, so the edge follows the pointer.
        SplitHandle {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            lineVisible: false
            property real from: 0
            property real scroll: 0
            property real content: 0
            property real anchorY: 0
            onStarted: {
                from = row.arrange.trackHeight;
                scroll = row.arrange.scrollY;
                content = row.arrange.contentHeight;
                anchorY = row.y - scroll + row.arrange.rulerHeight;
            }
            onMoved: (delta, done) => row.arrange.resizeTracks(
                from + row.edgeChange(delta, scroll, row.index, 1, Tracks.count, content, row.visibleHeight()), anchorY)
            onCanceled: row.arrange.resizeTracks(from, anchorY)
            onReset: row.arrange.resizeTracks(row.arrange.defaultTrackHeight(), row.y - row.arrange.scrollY + row.arrange.rulerHeight)
            Accessible.ignored: true
        }
    }

    // The rows' headers, one per row the timeline shows under the track.
    Repeater {
        id: rowHeaders
        model: row.expanded ? row.rows.length : 0
        delegate: RowHeader {
            required index
            width: row.width
            height: row.arrange.laneHeight
            track: row.index
            strip: row.strip
            hasInstrument: row.instrument !== ""
            info: row.rows[index] ?? null

            // Drag the lower edge to resize every automation lane,
            // double-click for the default. This track's top stays put and
            // the lanes above the edge share the movement (`ratio`), so the
            // edge follows the pointer.
            SplitHandle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                lineVisible: false
                property real from: 0
                property real scroll: 0
                property real content: 0
                property int above: 0
                // Lane rows in the whole timeline.
                property int total: 0
                function start(): void {
                    from = row.arrange.laneHeight;
                    scroll = row.arrange.scrollY;
                    content = row.arrange.contentHeight;
                    above = row.lanesAbove();
                    total = from > 0 ? Math.round((content - Tracks.count * row.arrange.trackHeight) / from) : 0;
                }
                // The track's top stays where it was, as far as the view
                // scrolls.
                function keepTop(): void {
                    row.arrange.scrollTo(row.arrange.scrollX, Math.max(0, scroll + above * (row.arrange.laneHeight - from)));
                }
                onStarted: {
                    start();
                    row.laneSize.begin(from, Infinity);
                }
                onMoved: (delta, done) => {
                    row.laneSize.drag(row.edgeChange(delta, scroll, above, index + 1, total, content, row.visibleHeight()), done);
                    keepTop();
                }
                onCanceled: {
                    row.laneSize.cancel();
                    keepTop();
                }
                onReset: {
                    start();
                    row.laneSize.reset();
                    keepTop();
                }
                Accessible.ignored: true
            }
        }
    }
}
