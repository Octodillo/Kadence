// SPDX-License-Identifier: GPL-3.0-or-later

pragma Singleton

import QtQuick

import io.github.octodillo.daw

// Words control values with their units, in the user's language, the same
// way wherever they show (the channel strip, the device panel, the
// automation lanes): levels, pan positions and device parameters.
// A singleton: ValueText.level(-6).
QtObject {
    function number(v: real, decimals: int): string {
        return v.toLocaleString(Qt.locale(), "f", decimals);
    }

    function signed(v: real, decimals: int): string {
        return (v > 0 ? "+" : "") + number(v, decimals);
    }

    // A fader or send level in dB, signed; at or below Mixer.silentDb it is
    // silence.
    function level(db: real): string {
        return db <= Mixer.silentDb ? i18nc("@label fader at silence", "−∞")
                                  : i18nc("@label decibels", "%1 dB", signed(Math.round(db * 10) / 10, 1));
    }

    // A pan position from -1 (left) to 1 (right).
    function pan(p: real): string {
        const pct = Math.round(Math.abs(p) * 100);
        if (pct === 0) {
            return i18nc("@label pan centre", "C");
        }
        return p < 0 ? i18nc("@label pan left, percent", "L%1", pct)
                     : i18nc("@label pan right, percent", "R%1", pct);
    }

    // A device parameter's value. `control` and `unit` are the names
    // ParamDesc::control_name and unit_name give; `choices` the labels of
    // a choice parameter.
    function param(value: real, control: string, unit: string, choices: var): string {
        switch (control) {
        case "toggle":
            return value >= 0.5 ? i18nc("@label parameter switched on", "On")
                                : i18nc("@label parameter switched off", "Off");
        case "choice":
            return DeviceNames.name(choices[Math.round(value)] ?? "");
        case "integer":
            return number(Math.round(value), 0);
        }
        switch (unit) {
        case "dB":
            return value <= DevicePanel.silentDb ? i18nc("@label silence in decibels", "−∞ dB")
                                          : i18nc("@label decibels", "%1 dB", number(value, 1));
        case "Hz":
            return value >= 1000 ? i18nc("@label kilohertz", "%1 kHz", number(value / 1000, 2))
                                 : i18nc("@label hertz", "%1 Hz", number(value, 0));
        case "ms":
            return value >= 1000 ? i18nc("@label seconds", "%1 s", number(value / 1000, 2))
                                 : i18nc("@label milliseconds", "%1 ms", number(value, 1));
        case "percent":
            return i18nc("@label percent", "%1 %", number(value * 100, 0));
        case "semitones":
            return i18nc("@label semitones", "%1 st", signed(Math.round(value), 0));
        case "cents":
            return i18nc("@label cents", "%1 ct", signed(Math.round(value), 0));
        case "octaves":
            return i18nc("@label octaves", "%1 oct", signed(value, 2));
        case "ratio":
            return i18nc("@label compression ratio", "%1:1", number(value, 1));
        case "pan":
            return pan(value);
        }
        return number(value, 2);
    }
}
