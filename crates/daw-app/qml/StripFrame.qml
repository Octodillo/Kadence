// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// What every mixer strip shares: its name and kind, a box (effects and
// sends on a channel) and its controls below. All strips have one width
// and one box height, the LayoutSizes the Mix page holds; dragging a
// strip's right edge or the line under its box changes them for all.
Item {
    id: frame

    required property LayoutSize widthSize
    required property LayoutSize boxSize
    // Strips before this one in the row: a width drag widens them too.
    property int position: 0
    property string title
    property string subtitle
    property bool dimmed: false
    property color color: Kirigami.Theme.backgroundColor
    // How much the controls can shrink, which the box may grow into.
    property real slack: 0

    // What the box shows; one item, which fills it.
    property alias box: boxArea.data
    default property alias controls: controls.data

    implicitWidth: widthSize.pixels
    width: implicitWidth

    Rectangle {
        anchors.fill: parent
        color: frame.color
    }
    Kirigami.Separator {
        anchors {
            top: parent.top
            bottom: parent.bottom
            right: parent.right
        }
    }
    // In the strip's right margin: the next strip is drawn over anything
    // past its edge.
    SplitHandle {
        anchors {
            top: parent.top
            bottom: parent.bottom
            right: parent.right
        }
        width: Kirigami.Units.smallSpacing + 1
        z: 1
        orientation: Qt.Vertical
        lineVisible: false
        ratio: frame.position + 1
        onStarted: frame.widthSize.begin(frame.width, Infinity)
        onMoved: (delta, done) => frame.widthSize.drag(delta, done)
        onCanceled: frame.widthSize.cancel()
        onReset: frame.widthSize.reset()
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
            text: frame.title
            elide: Text.ElideRight
            horizontalAlignment: Text.AlignHCenter
            opacity: frame.dimmed ? 0.6 : 1
            HoverHandler {
                id: titleHover
            }
            // The whole name, when the strip is too narrow for it.
            QQC2.ToolTip.text: text
            QQC2.ToolTip.visible: titleHover.hovered && truncated
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }
        QQC2.Label {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            elide: Text.ElideRight
            font: Kirigami.Theme.smallFont
            color: Kirigami.Theme.disabledTextColor
            text: frame.subtitle
        }

        Item {
            id: boxArea
            Layout.fillWidth: true
            Layout.preferredHeight: frame.boxSize.pixels
            Layout.minimumHeight: frame.boxSize.minimumPixels
        }
        SplitHandle {
            Layout.fillWidth: true
            Layout.topMargin: -Kirigami.Units.smallSpacing
            Layout.bottomMargin: -Kirigami.Units.smallSpacing
            onStarted: frame.boxSize.begin(boxArea.height, boxArea.height + Math.max(0, frame.slack))
            onMoved: (delta, done) => frame.boxSize.drag(delta, done)
            onCanceled: frame.boxSize.cancel()
            onReset: frame.boxSize.reset()
        }

        ColumnLayout {
            id: controls
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: Kirigami.Units.smallSpacing
        }
    }
}
