// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Dialogs
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Settings that are about neither audio nor MIDI. Changes are saved at once.
Kirigami.FormLayout {
    id: settings

    RowLayout {
        Kirigami.FormData.label: i18nc("@label", "Project folder:")
        spacing: Kirigami.Units.smallSpacing

        QQC2.TextField {
            Layout.fillWidth: true
            Layout.minimumWidth: Kirigami.Units.gridUnit * 10
            readOnly: true
            text: AppSettings.projectFolder
            placeholderText: i18nc("@info:placeholder", "Not set")
            Accessible.name: i18nc("@label", "Project folder")
        }
        QQC2.Button {
            text: i18nc("@action:button", "Choose…")
            icon.name: "document-open-folder"
            onClicked: folderDialog.open()
        }
        QQC2.ToolButton {
            text: i18nc("@action:button", "Clear")
            icon.name: "edit-clear"
            display: QQC2.AbstractButton.IconOnly
            enabled: AppSettings.projectFolder !== ""
            onClicked: AppSettings.chooseProjectFolder("")
            QQC2.ToolTip.text: text
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }
    }

    QQC2.Label {
        Layout.fillWidth: true
        Layout.maximumWidth: Kirigami.Units.gridUnit * 20
        text: i18nc("@info", "Where Open and Save As start.")
        wrapMode: Text.Wrap
        font: Kirigami.Theme.smallFont
        opacity: 0.7
    }

    QQC2.CheckBox {
        id: autosaveBox
        Kirigami.FormData.label: i18nc("@label", "Autosave:")
        text: i18nc("@option:check", "Save a recovery copy of changed projects")
        checked: AppSettings.autosaveMinutes > 0
        onToggled: AppSettings.chooseAutosaveMinutes(checked ? Math.max(1, minutesBox.value) : 0)
    }

    QQC2.SpinBox {
        id: minutesBox
        Kirigami.FormData.label: i18nc("@label:spinbox", "Every:")
        enabled: autosaveBox.checked
        from: 1
        to: AppSettings.maxAutosaveMinutes
        value: Math.max(1, AppSettings.autosaveMinutes)
        textFromValue: (value, locale) => i18ncp("@item:valuesuffix autosave interval", "%1 minute", "%1 minutes", value)
        valueFromText: (text, locale) => parseInt(text, 10) || 1
        onValueModified: AppSettings.chooseAutosaveMinutes(value)
    }

    QQC2.CheckBox {
        Kirigami.FormData.label: i18nc("@label", "Piano roll:")
        text: i18nc("@option:check", "Play keys and notes as they are pressed or moved")
        checked: AppSettings.previewNotes
        onToggled: AppSettings.choosePreviewNotes(checked)
    }

    FolderDialog {
        id: folderDialog
        title: i18nc("@title:window", "Choose Project Folder")
        currentFolder: AppSettings.projectFolderUrl
        onAccepted: AppSettings.chooseProjectFolder(selectedFolder)
    }
}
