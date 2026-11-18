// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

// The keyboard's way to a context menu: Menu or Shift+F10 asks for it, as
// a right click or a long press does with a pointer. The item that has the
// menu forwards its keys here (`Keys.forwardTo`) and opens the menu on
// `requested`.
Item {
    signal requested()

    Keys.onMenuPressed: requested()
    Keys.onPressed: (event) => {
        if (event.key === Qt.Key_F10 && (event.modifiers & Qt.ShiftModifier)) {
            requested();
            event.accepted = true;
        }
    }
}
