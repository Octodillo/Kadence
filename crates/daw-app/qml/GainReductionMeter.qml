// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

// A horizontal bar growing from the right as a compressor reduces gain,
// 0 to 24 dB.
Rectangle {
    id: meter

    // Gain reduction in dB, positive.
    property real reduction: 0
    readonly property real range: 24
    property real shown: 0

    implicitHeight: Kirigami.Units.smallSpacing * 2
    color: Kirigami.Theme.alternateBackgroundColor
    border.color: Kirigami.ColorUtils.linearInterpolation(Kirigami.Theme.backgroundColor, Kirigami.Theme.textColor, 0.15)
    border.width: 1

    onReductionChanged: shown = Math.max(Math.min(reduction / range, 1), shown - 0.02)

    Accessible.role: Accessible.Indicator
    Accessible.name: i18nc("@label", "Gain reduction meter")

    Rectangle {
        anchors {
            top: parent.top
            bottom: parent.bottom
            right: parent.right
            margins: 1
        }
        width: (parent.width - 2) * meter.shown
        color: Kirigami.Theme.neutralTextColor
    }
}
