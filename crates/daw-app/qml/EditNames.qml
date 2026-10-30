// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

// Translates the names of edits for the Undo and Redo menu items. The
// model names its commands in English (like "Move Clip"), which serve as
// keys here; tests/edit_names.rs checks that every name in the Rust code
// has an entry. An unknown name is shown as it is.
QtObject {
    readonly property var names: ({
        "Add Automation Lane": i18nc("@item:inmenu name of an edit", "Add Automation Lane"),
        "Add Clip": i18nc("@item:inmenu name of an edit", "Add Clip"),
        "Add Device": i18nc("@item:inmenu name of an edit", "Add Device"),
        "Add Note": i18nc("@item:inmenu name of an edit", "Add Note"),
        "Add Notes": i18nc("@item:inmenu name of an edit", "Add Notes"),
        "Add Send": i18nc("@item:inmenu name of an edit", "Add Send"),
        "Add Track": i18nc("@item:inmenu name of an edit", "Add Track"),
        "Bypass Device": i18nc("@item:inmenu name of an edit", "Bypass Device"),
        "Change Clip Gain": i18nc("@item:inmenu name of an edit", "Change Clip Gain"),
        "Change Fade In": i18nc("@item:inmenu name of an edit", "Change Fade In"),
        "Change Fade Out": i18nc("@item:inmenu name of an edit", "Change Fade Out"),
        "Change Fade Shape": i18nc("@item:inmenu name of an edit", "Change Fade Shape"),
        "Change Instrument": i18nc("@item:inmenu name of an edit", "Change Instrument"),
        "Change Pan": i18nc("@item:inmenu name of an edit", "Change Pan"),
        "Change Parameter": i18nc("@item:inmenu name of an edit", "Change Parameter"),
        "Change Plugin State": i18nc("@item:inmenu name of an edit", "Change Plugin State"),
        "Change Send Level": i18nc("@item:inmenu name of an edit", "Change Send Level"),
        "Change Send Tap": i18nc("@item:inmenu name of an edit", "Change Send Tap"),
        "Change Send Target": i18nc("@item:inmenu name of an edit", "Change Send Target"),
        "Change Tempo": i18nc("@item:inmenu name of an edit", "Change Tempo"),
        "Change Track Color": i18nc("@item:inmenu name of an edit", "Change Track Color"),
        "Change Track Output": i18nc("@item:inmenu name of an edit", "Change Track Output"),
        "Change Volume": i18nc("@item:inmenu name of an edit", "Change Volume"),
        "Delete Clip": i18nc("@item:inmenu name of an edit", "Delete Clip"),
        "Delete Clips": i18nc("@item:inmenu name of an edit", "Delete Clips"),
        "Delete Note": i18nc("@item:inmenu name of an edit", "Delete Note"),
        "Delete Notes": i18nc("@item:inmenu name of an edit", "Delete Notes"),
        "Duplicate": i18nc("@item:inmenu name of an edit", "Duplicate"),
        "Duplicate Notes": i18nc("@item:inmenu name of an edit", "Duplicate Notes"),
        "Edit Automation": i18nc("@item:inmenu name of an edit", "Edit Automation"),
        "Edit Note": i18nc("@item:inmenu name of an edit", "Edit Note"),
        "Edit Notes": i18nc("@item:inmenu name of an edit", "Edit Notes"),
        "Fix Overlaps": i18nc("@item:inmenu name of an edit", "Fix Overlaps"),
        "Glue Notes": i18nc("@item:inmenu name of an edit", "Glue Notes"),
        "Humanize": i18nc("@item:inmenu name of an edit", "Humanize"),
        "Import Audio": i18nc("@item:inmenu name of an edit", "Import Audio"),
        "Legato": i18nc("@item:inmenu name of an edit", "Legato"),
        "Move Clip": i18nc("@item:inmenu name of an edit", "Move Clip"),
        "Move Clips": i18nc("@item:inmenu name of an edit", "Move Clips"),
        "Move Device": i18nc("@item:inmenu name of an edit", "Move Device"),
        "Move Track": i18nc("@item:inmenu name of an edit", "Move Track"),
        "Mute Clip": i18nc("@item:inmenu name of an edit", "Mute Clip"),
        "Mute Track": i18nc("@item:inmenu name of an edit", "Mute Track"),
        "Nudge": i18nc("@item:inmenu name of an edit", "Nudge"),
        "Quantize": i18nc("@item:inmenu name of an edit", "Quantize"),
        "Record Notes": i18nc("@item:inmenu name of an edit", "Record Notes"),
        "Remove Automation Lane": i18nc("@item:inmenu name of an edit", "Remove Automation Lane"),
        "Remove Clip": i18nc("@item:inmenu name of an edit", "Remove Clip"),
        "Remove Device": i18nc("@item:inmenu name of an edit", "Remove Device"),
        "Remove Fades": i18nc("@item:inmenu name of an edit", "Remove Fades"),
        "Remove Send": i18nc("@item:inmenu name of an edit", "Remove Send"),
        "Remove Track": i18nc("@item:inmenu name of an edit", "Remove Track"),
        "Rename Clip": i18nc("@item:inmenu name of an edit", "Rename Clip"),
        "Rename Device": i18nc("@item:inmenu name of an edit", "Rename Device"),
        "Rename Project": i18nc("@item:inmenu name of an edit", "Rename Project"),
        "Rename Track": i18nc("@item:inmenu name of an edit", "Rename Track"),
        "Reset Clip Gain": i18nc("@item:inmenu name of an edit", "Reset Clip Gain"),
        "Resize Clip": i18nc("@item:inmenu name of an edit", "Resize Clip"),
        "Reverse": i18nc("@item:inmenu name of an edit", "Reverse"),
        "Scale Velocity": i18nc("@item:inmenu name of an edit", "Scale Velocity"),
        "Set Velocity": i18nc("@item:inmenu name of an edit", "Set Velocity"),
        "Slip Clip": i18nc("@item:inmenu name of an edit", "Slip Clip"),
        "Solo Track": i18nc("@item:inmenu name of an edit", "Solo Track"),
        "Split": i18nc("@item:inmenu name of an edit", "Split"),
        "Split Notes": i18nc("@item:inmenu name of an edit", "Split Notes"),
        "Toggle Automation": i18nc("@item:inmenu name of an edit", "Toggle Automation"),
        "Transpose": i18nc("@item:inmenu name of an edit", "Transpose"),
        "Write Automation": i18nc("@item:inmenu name of an edit", "Write Automation")
    })

    function name(label: string): string {
        return names[label] ?? label;
    }
}
