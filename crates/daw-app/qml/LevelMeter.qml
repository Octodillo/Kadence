// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

// A vertical stereo peak meter, -60 to +6 dBFS. Positions come in ready to
// draw (ballistics and peak hold run in Rust, see MeterBallistics).
Item {
    id: meter

    // Per channel, 0 (-60 dB) to 1 (+6 dB).
    property list<real> levels: [0, 0]
    property list<real> holds: [0, 0]

    readonly property real floorDb: -60
    readonly property real topDb: 6
    readonly property real span: topDb - floorDb

    implicitWidth: Kirigami.Units.smallSpacing * 4 + 1

    Accessible.role: Accessible.Indicator
    Accessible.name: i18nc("@label", "Level meter")

    Row {
        anchors.fill: parent
        spacing: 1
        Repeater {
            model: 2
            delegate: Rectangle {
                id: bar
                required property int index
                readonly property real level: meter.levels[index] ?? 0
                readonly property real held: meter.holds[index] ?? 0

                width: (meter.width - 1) / 2
                height: meter.height
                color: Kirigami.Theme.alternateBackgroundColor
                border.color: Kirigami.ColorUtils.linearInterpolation(Kirigami.Theme.backgroundColor, Kirigami.Theme.textColor, 0.15)
                border.width: 1

                // The lit part: fixed colour zones, cut to the current level.
                // Green below -12 dB, amber up to 0 dB, red above.
                Item {
                    anchors {
                        left: parent.left
                        right: parent.right
                        bottom: parent.bottom
                        margins: 1
                    }
                    height: (bar.height - 2) * bar.level
                    clip: true
                    Column {
                        anchors {
                            left: parent.left
                            right: parent.right
                            bottom: parent.bottom
                        }
                        readonly property real full: bar.height - 2
                        Rectangle {
                            width: parent.width
                            height: parent.full * meter.topDb / meter.span
                            color: Kirigami.Theme.negativeTextColor
                        }
                        Rectangle {
                            width: parent.width
                            height: parent.full * 12 / meter.span
                            color: Kirigami.Theme.neutralTextColor
                        }
                        Rectangle {
                            width: parent.width
                            height: parent.full * (-12 - meter.floorDb) / meter.span
                            color: Kirigami.Theme.positiveTextColor
                        }
                    }
                }
                Rectangle {
                    visible: bar.held > 0
                    x: 1
                    width: parent.width - 2
                    height: 1
                    y: 1 + (parent.height - 2) * (1 - bar.held)
                    color: bar.held > -meter.floorDb / meter.span ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.textColor
                }
            }
        }
    }
}
