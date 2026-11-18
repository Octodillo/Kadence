// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

import io.github.octodillo.daw

// Words what a row under a track is: its kind (an Automation.LaneKind: a
// control, or the heading of a group of them), the bus or device, and the
// parameter.
QtObject {
    function text(kind: int, detail: string, param: string): string {
        switch (kind) {
        case Automation.VolumeLane:
            return i18nc("@label a track's control", "Volume");
        case Automation.PanLane:
            return i18nc("@label a track's control", "Pan");
        case Automation.SendLane:
            return detail === ""
                ? i18nc("@label a track's control", "Send")
                : i18nc("@label a track's control, %1 is the bus the send goes to", "Send to %1", detail);
        case Automation.DeviceParamLane:
            // Under its device's heading a parameter goes by its own name.
            return detail === ""
                ? DeviceNames.name(param)
                : i18nc("@label a track's control, %1 is a device, %2 one of its parameters", "%1: %2", DeviceNames.name(detail), DeviceNames.name(param));
        case Automation.InstrumentGroup:
            return i18nc("@title:group a track's controls", "Instrument");
        case Automation.EffectsGroup:
            return i18nc("@title:group a track's controls", "Effects");
        case Automation.SendsGroup:
            return i18nc("@title:group a track's controls", "Sends");
        case Automation.DeviceGroup:
            return DeviceNames.name(detail);
        case Automation.MoreGroup:
            return i18nc("@title:group a device's parameters past the first few", "More Parameters");
        default:
            return "";
        }
    }
}
