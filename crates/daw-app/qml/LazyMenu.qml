// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2

// A menu that is only built while it is open. Every track header carries
// several menus, and building them all whenever a header scrolls into view
// makes scrolling a long track list stutter. Give it the menu as `menu`;
// call popup() and dismiss() as on a Menu.
Loader {
    id: loader

    property Component menu
    readonly property bool opened: item ? (item as QQC2.Menu).visible : false

    active: false
    sourceComponent: menu

    // Under `at`, offset by x and y; or at the pointer when `at` is left out.
    function popup(at, x, y): void {
        active = true;
        const menu = item as QQC2.Menu;
        // Place the menu in the window of whatever holds this loader.
        menu.parent = loader.parent;
        if (at === undefined) {
            menu.popup();
        } else {
            menu.popup(at, x, y);
        }
    }

    function dismiss(): void {
        active = false;
    }

    Connections {
        target: loader.item
        // Not during the menu's own signal: let it finish closing first.
        function onClosed(): void {
            Qt.callLater(() => {
                if (!loader.opened) {
                    loader.active = false;
                }
            });
        }
    }
}
