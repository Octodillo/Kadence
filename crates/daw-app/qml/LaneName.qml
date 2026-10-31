// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

// Words what an automation lane controls, from Automation's lane or target
// getters: the kind (0 volume, 1 pan, 2 send, 3 device parameter), the bus
// or device, and the parameter.
QtObject {
    function text(kind: int, detail: string, param: string): string {
        switch (kind) {
        case 0:
            return i18nc("@label automation lane", "Volume");
        case 1:
            return i18nc("@label automation lane", "Pan");
        case 2:
            return detail === ""
                ? i18nc("@label automation lane", "Send")
                : i18nc("@label automation lane, %1 is the bus the send goes to", "Send to %1", detail);
        case 3:
            return i18nc("@label automation lane, %1 is a device, %2 one of its parameters", "%1: %2", detail, param);
        default:
            return "";
        }
    }
}
