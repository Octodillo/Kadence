// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Dialogs
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Where plugins are looked for, and how the last scan went. Folder changes
// are saved and rescanned at once.
Kirigami.FormLayout {
    id: settings

    Kirigami.Separator {
        Kirigami.FormData.isSection: true
        Kirigami.FormData.label: i18nc("@title:group", "Plugins")
    }

    RowLayout {
        Kirigami.FormData.label: i18nc("@label", "Installed:")
        spacing: Kirigami.Units.smallSpacing

        QQC2.Label {
            text: Plugins.scanning
                ? (Plugins.scanTotal > 0
                   ? i18nc("@info:status %1 and %2 are numbers", "Scanning plugins… %1 of %2", Plugins.scanDone, Plugins.scanTotal)
                   : i18nc("@info:status", "Scanning plugins…"))
                : i18ncp("@info:status", "%1 plugin", "%1 plugins", Plugins.installedCount)
        }
        QQC2.BusyIndicator {
            visible: Plugins.scanning
            running: visible
            Layout.preferredHeight: Kirigami.Units.iconSizes.smallMedium
            Layout.preferredWidth: Layout.preferredHeight
        }
    }

    QQC2.Button {
        icon.name: "view-refresh"
        text: i18nc("@action:button scan for plugins again", "Rescan")
        enabled: !Plugins.scanning
        QQC2.ToolTip.text: i18nc("@info:tooltip", "Look for new, updated or removed plugins, and try again those that failed")
        QQC2.ToolTip.visible: hovered
        QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        onClicked: Plugins.rescan()
    }

    ColumnLayout {
        Kirigami.FormData.label: i18nc("@label", "Other folders:")
        Kirigami.FormData.labelAlignment: Qt.AlignTop
        spacing: 0

        QQC2.Label {
            visible: Plugins.extraFolders.length === 0
            text: i18nc("@info", "None. CLAP, VST3 and LV2 plugins are found in their usual folders.")
            wrapMode: Text.Wrap
            Layout.maximumWidth: Kirigami.Units.gridUnit * 16
            opacity: 0.7
        }
        Repeater {
            model: Plugins.extraFolders
            delegate: RowLayout {
                id: folderRow
                required property int index
                required property string modelData
                spacing: 0
                QQC2.Label {
                    text: folderRow.modelData
                    elide: Text.ElideMiddle
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 14
                }
                QQC2.ToolButton {
                    icon.name: "list-remove"
                    display: QQC2.AbstractButton.IconOnly
                    text: i18nc("@action:button %1 is a folder", "Remove %1", folderRow.modelData)
                    QQC2.ToolTip.text: text
                    QQC2.ToolTip.visible: hovered
                    QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                    onClicked: Plugins.removeFolder(folderRow.index)
                }
            }
        }
        QQC2.Button {
            icon.name: "folder-add"
            text: i18nc("@action:button", "Add Folder…")
            onClicked: folderDialog.open()
        }
    }

    ColumnLayout {
        Kirigami.FormData.label: i18nc("@label", "Not loaded:")
        Kirigami.FormData.labelAlignment: Qt.AlignTop
        visible: Plugins.failedBundles.length > 0
        spacing: Kirigami.Units.smallSpacing

        Repeater {
            model: Plugins.failedBundles
            delegate: ColumnLayout {
                id: failure
                required property int index
                required property string modelData
                spacing: 0
                QQC2.Label {
                    // The file name; the full path is in the tooltip.
                    text: failure.modelData.substring(failure.modelData.lastIndexOf("/") + 1)
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 16
                    elide: Text.ElideMiddle
                    QQC2.ToolTip.text: failure.modelData
                    QQC2.ToolTip.visible: failureHover.hovered
                    HoverHandler { id: failureHover }
                }
                QQC2.Label {
                    // The plugin's or the format's own words.
                    text: Plugins.failureReasons[failure.index] ?? ""
                    font: Kirigami.Theme.smallFont
                    wrapMode: Text.Wrap
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 16
                    opacity: 0.7
                }
            }
        }
    }

    FolderDialog {
        id: folderDialog
        title: i18nc("@title:window", "Add Plugin Folder")
        onAccepted: Plugins.addFolder(selectedFolder)
    }
}
