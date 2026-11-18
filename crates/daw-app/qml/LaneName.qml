// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

import io.github.octodillo.daw

// Words what an automation lane controls: the kind (an Automation.LaneKind),
// the bus or device, and the parameter.
QtObject {
    function text(kind: int, detail: string, param: string): string {
        switch (kind) {
        case Automation.VolumeLane:
            return i18nc("@label automation lane", "Volume");
        case Automation.PanLane:
            return i18nc("@label automation lane", "Pan");
        case Automation.SendLane:
            return detail === ""
                ? i18nc("@label automation lane", "Send")
                : i18nc("@label automation lane, %1 is the bus the send goes to", "Send to %1", detail);
        case Automation.DeviceParamLane:
            return i18nc("@label automation lane, %1 is a device, %2 one of its parameters", "%1: %2", DeviceNames.name(detail), DeviceNames.name(param));
        default:
            return "";
        }
    }
}
