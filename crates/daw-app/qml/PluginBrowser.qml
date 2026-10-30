// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Finds an installed plugin: search by name or vendor, filter by type and
// format. Picking one emits `chosen` and closes. The list is the Plugins
// model, which a background scan fills.
Kirigami.Dialog {
    id: browser

    // What the opener wants: 0 any plugin, 1 instruments, 2 effects. The
    // user can change the type filter from there.
    property int kind: 0

    // `format` is a Plugins `formatId`, as Mixer.addPluginInsert takes it.
    signal chosen(string format, string pluginId, string name)
    // The user wants to change where plugins are looked for.
    signal configureRequested()

    title: kind === 1 ? i18nc("@title:window", "Choose Instrument Plugin")
         : kind === 2 ? i18nc("@title:window", "Add Effect Plugin")
         : i18nc("@title:window", "Plugins")
    preferredWidth: Kirigami.Units.gridUnit * 30
    preferredHeight: Kirigami.Units.gridUnit * 28
    padding: 0
    standardButtons: QQC2.Dialog.NoButton

    function refilter(): void {
        Plugins.setFilter(search.text, formatBox.currentIndex, typeBox.currentIndex);
    }

    onAboutToShow: {
        search.text = "";
        typeBox.currentIndex = browser.kind;
        formatBox.currentIndex = 0;
        refilter();
    }
    onOpened: search.forceActiveFocus()

    function pick(row: int): void {
        list.currentIndex = row;
        const item = list.itemAtIndex(row);
        if (item) {
            browser.chosen(item.formatId, item.pluginId, item.name);
            browser.close();
        }
    }

    ColumnLayout {
        spacing: 0

        RowLayout {
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            spacing: Kirigami.Units.smallSpacing

            Kirigami.SearchField {
                id: search
                Layout.fillWidth: true
                onTextChanged: browser.refilter()
                // Enter takes the first match.
                onAccepted: if (list.count > 0) browser.pick(0)
                KeyNavigation.down: list
            }
            QQC2.ComboBox {
                id: typeBox
                Accessible.name: i18nc("@label:listbox", "Plugin type")
                model: [
                    i18nc("@item:inlistbox plugin type filter", "All Types"),
                    i18nc("@item:inlistbox plugin type filter", "Instruments"),
                    i18nc("@item:inlistbox plugin type filter", "Effects")
                ]
                onActivated: browser.refilter()
            }
            QQC2.ComboBox {
                id: formatBox
                Accessible.name: i18nc("@label:listbox", "Plugin format")
                // Order matches Plugins.setFilter. CLAP, VST3 and LV2 are names.
                model: [i18nc("@item:inlistbox plugin format filter", "All Formats"), "CLAP", "VST3", "LV2"]
                onActivated: browser.refilter()
            }
        }

        Kirigami.Separator {
            Layout.fillWidth: true
        }

        QQC2.ScrollView {
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.minimumHeight: Kirigami.Units.gridUnit * 10

            ListView {
                id: list
                model: Plugins
                clip: true
                keyNavigationWraps: true
                Keys.onReturnPressed: if (currentIndex >= 0) browser.pick(currentIndex)
                Keys.onEnterPressed: if (currentIndex >= 0) browser.pick(currentIndex)

                delegate: QQC2.ItemDelegate {
                    id: row
                    required property int index
                    required property string name
                    required property string vendor
                    required property string format
                    required property string formatId
                    required property string pluginId
                    required property bool instrument

                    width: ListView.view.width
                    text: name
                    highlighted: ListView.isCurrentItem
                    onClicked: browser.pick(index)

                    contentItem: RowLayout {
                        spacing: Kirigami.Units.largeSpacing
                        Kirigami.Icon {
                            source: row.instrument ? "audio-midi" : "view-media-equalizer"
                            Layout.preferredWidth: Kirigami.Units.iconSizes.smallMedium
                            Layout.preferredHeight: Kirigami.Units.iconSizes.smallMedium
                        }
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 0
                            QQC2.Label {
                                Layout.fillWidth: true
                                text: row.name
                                elide: Text.ElideRight
                                color: row.highlighted ? Kirigami.Theme.highlightedTextColor : Kirigami.Theme.textColor
                            }
                            QQC2.Label {
                                Layout.fillWidth: true
                                text: row.vendor !== ""
                                    ? i18nc("@info plugin vendor and format, as in “Surge Synth Team · CLAP”", "%1 · %2", row.vendor, row.format)
                                    : row.format
                                elide: Text.ElideRight
                                font: Kirigami.Theme.smallFont
                                color: row.highlighted ? Kirigami.Theme.highlightedTextColor : Kirigami.Theme.disabledTextColor
                            }
                        }
                    }
                }

                Kirigami.PlaceholderMessage {
                    anchors.centerIn: parent
                    width: parent.width - Kirigami.Units.gridUnit * 4
                    visible: list.count === 0
                    icon.name: !Plugins.hasScanned ? "view-refresh" : "preferences-plugin"
                    text: !Plugins.hasScanned
                        ? i18nc("@info:placeholder", "Looking for plugins…")
                        : Plugins.installedCount === 0
                        ? i18nc("@info:placeholder", "No plugins found")
                        : i18nc("@info:placeholder", "No matching plugins")
                    explanation: Plugins.hasScanned && Plugins.installedCount === 0
                        ? i18nc("@info:placeholder",
                                 "Kadence looks for CLAP, VST3 and LV2 plugins in their usual folders. Add other folders in the plugin settings.")
                        : ""
                    helpfulAction: Plugins.hasScanned && Plugins.installedCount === 0 ? foldersAction : null
                }
            }
        }
    }

    Kirigami.Action {
        id: foldersAction
        icon.name: "folder-open"
        text: i18nc("@action:button", "Plugin Folders…")
        onTriggered: {
            browser.close();
            browser.configureRequested();
        }
    }

    footer: QQC2.ToolBar {
        position: QQC2.ToolBar.Footer
        contentItem: RowLayout {
            spacing: Kirigami.Units.smallSpacing

            QQC2.ProgressBar {
                Layout.preferredWidth: Kirigami.Units.gridUnit * 6
                visible: Plugins.scanning
                indeterminate: Plugins.scanTotal === 0
                from: 0
                to: Math.max(1, Plugins.scanTotal)
                value: Plugins.scanDone
            }
            QQC2.Label {
                Layout.fillWidth: true
                elide: Text.ElideRight
                text: Plugins.scanning
                    ? (Plugins.scanTotal > 0
                       ? i18nc("@info:status %1 and %2 are numbers", "Scanning plugins… %1 of %2", Plugins.scanDone, Plugins.scanTotal)
                       : i18nc("@info:status", "Scanning plugins…"))
                    : i18ncp("@info:status", "%1 plugin installed", "%1 plugins installed", Plugins.installedCount)
            }
            QQC2.ToolButton {
                icon.name: "view-refresh"
                text: i18nc("@action:button scan for plugins again", "Rescan")
                enabled: !Plugins.scanning
                onClicked: Plugins.rescan()
            }
        }
    }
}
