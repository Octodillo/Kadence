// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2

import io.github.octodillo.daw

// The effects that can be added to the Mixer's strip `row`: the built-in
// ones, then the plugin browser. Picking one adds it at the end of the
// strip's chain.
QQC2.Menu {
    id: menu

    required property int row

    Repeater {
        model: Mixer.effectNames
        delegate: QQC2.MenuItem {
            required property int index
            required property string modelData
            text: modelData
            onTriggered: Mixer.addInsert(menu.row, index)
        }
    }
    QQC2.MenuSeparator {}
    QQC2.MenuItem {
        icon.name: "preferences-plugin"
        text: i18nc("@action:inmenu", "Browse Plugins…")
        onTriggered: {
            // The strip may move while the browser is open.
            const row = menu.row;
            applicationWindow().browsePlugins(2, (format, pluginId, name) => {
                Mixer.addPluginInsert(row, format, pluginId, name);
            });
        }
    }
}
