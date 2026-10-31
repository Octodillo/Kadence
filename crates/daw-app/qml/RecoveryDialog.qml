// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Offers the unsaved work that earlier sessions left behind when Kadence
// quit without saving (Project.recoveryCount entries), newest first.
Kirigami.Dialog {
    id: dialog

    // Emitted with what Project.recoverSession returned.
    signal recovered(var result)

    title: i18nc("@title:window", "Recover Unsaved Work")
    standardButtons: Kirigami.Dialog.Close
    preferredWidth: Kirigami.Units.gridUnit * 26
    padding: Kirigami.Units.largeSpacing

    // Nothing left to decide.
    Connections {
        target: Project
        function onRecoveryCountChanged(): void {
            if (Project.recoveryCount === 0) {
                dialog.close();
            }
        }
    }

    ColumnLayout {
        spacing: Kirigami.Units.largeSpacing

        QQC2.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            text: i18ncp("@info",
                         "Kadence did not close properly last time. This project has changes that were never saved:",
                         "Kadence did not close properly last time. These %1 projects have changes that were never saved:",
                         Project.recoveryCount)
        }

        Repeater {
            // Rebuilt whenever an entry is recovered or discarded.
            model: Project.recoveryCount

            delegate: RowLayout {
                id: row
                required property int index
                readonly property string name: Project.recoveryName(index)
                readonly property real time: Project.recoveryTime(index)

                Layout.fillWidth: true
                spacing: Kirigami.Units.smallSpacing

                Kirigami.Icon {
                    source: "document-revert"
                    Layout.preferredWidth: Kirigami.Units.iconSizes.medium
                    Layout.preferredHeight: Kirigami.Units.iconSizes.medium
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0

                    QQC2.Label {
                        Layout.fillWidth: true
                        elide: Text.ElideRight
                        text: row.name !== "" ? row.name
                                              : i18nc("@item a project that was never saved", "Untitled project")
                    }
                    QQC2.Label {
                        Layout.fillWidth: true
                        elide: Text.ElideRight
                        font: Kirigami.Theme.smallFont
                        color: Kirigami.Theme.disabledTextColor
                        visible: row.time > 0
                        text: i18nc("@info %1 is a date and time", "Last changed %1",
                                    new Date(row.time).toLocaleString(Qt.locale(), Locale.ShortFormat))
                    }
                }

                QQC2.Button {
                    text: i18nc("@action:button", "Recover")
                    icon.name: "document-revert"
                    onClicked: {
                        const result = Project.recoverSession(row.index);
                        dialog.close();
                        dialog.recovered(result);
                    }
                }
                QQC2.Button {
                    text: i18nc("@action:button", "Discard")
                    icon.name: "edit-delete"
                    QQC2.ToolTip.text: i18nc("@info:tooltip", "Delete these unsaved changes for good")
                    QQC2.ToolTip.visible: hovered
                    QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                    onClicked: Project.discardSession(row.index)
                }
            }
        }

        QQC2.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            font: Kirigami.Theme.smallFont
            color: Kirigami.Theme.disabledTextColor
            text: i18nc("@info", "Anything you close this without choosing is offered again next time.")
        }
    }
}
