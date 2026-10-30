// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Audio device settings. Changes apply (and are saved) as soon as they are
// made; if the engine refuses one, the old setting stays and we say why.
ColumnLayout {
    id: settings

    spacing: Kirigami.Units.largeSpacing

    function apply(): void {
        const ok = Engine.applyAudioSettings(deviceBox.currentIndex,
                                             rateBox.currentValue,
                                             bufferBox.currentValue);
        if (!ok) {
            // Show what is actually running again.
            settings.syncFromEngine();
        }
    }

    function syncFromEngine(): void {
        deviceBox.currentIndex = Engine.deviceIndex;
        rateBox.currentIndex = rateBox.indexOfValue(Engine.sampleRate);
        bufferBox.currentIndex = bufferBox.indexOfValue(Engine.bufferSize);
    }

    Component.onCompleted: syncFromEngine()

    readonly property string backendLabel: {
        switch (Engine.backendKind) {
        case Engine.JackBackend:
            return i18nc("@info audio system", "JACK (PipeWire)");
        case Engine.SilentBackend:
            return i18nc("@info audio system", "None (no audio server, playing silently)");
        }
        return i18nc("@info audio system", "Test (no audio)");
    }

    // The name of the backend's own output, which has no name of its own.
    readonly property string backendDevice: {
        switch (Engine.backendKind) {
        case Engine.JackBackend:
            return i18nc("@item:inlistbox audio device", "JACK / PipeWire output");
        case Engine.SilentBackend:
            return i18nc("@item:inlistbox audio device", "No audio server (silent)");
        }
        return i18nc("@item:inlistbox audio device", "Test output");
    }

    EngineErrorText {
        id: engineError
    }

    Kirigami.InlineMessage {
        Layout.fillWidth: true
        // A fallback at startup worked, so audio runs: a warning, not an error.
        type: Engine.errorContext === Engine.StartupFallback ? Kirigami.MessageType.Warning
                                                             : Kirigami.MessageType.Error
        visible: engineError.active
        text: engineError.text
    }

    Kirigami.FormLayout {
        Layout.fillWidth: true

        QQC2.Label {
            Kirigami.FormData.label: i18nc("@label", "Audio system:")
            text: settings.backendLabel
        }

        QQC2.Label {
            Kirigami.FormData.label: i18nc("@label", "Status:")
            text: Engine.running ? i18nc("@info audio engine status", "Running")
                                 : i18nc("@info audio engine status", "Stopped")
        }

        Kirigami.Separator {
            Kirigami.FormData.isSection: true
        }

        QQC2.ComboBox {
            id: deviceBox
            Kirigami.FormData.label: i18nc("@label:listbox", "Device:")
            model: Engine.deviceNames.map((name, i) => i === 0 ? i18nc("@item:inlistbox audio device", "System default")
                                                     : name !== "" ? name : settings.backendDevice)
            onActivated: settings.apply()
        }

        QQC2.ComboBox {
            id: rateBox
            Kirigami.FormData.label: i18nc("@label:listbox", "Sample rate:")
            textRole: "text"
            valueRole: "value"
            model: Engine.sampleRates.map(rate => ({
                value: rate,
                text: i18nc("@item:inlistbox sample rate", "%1 Hz", rate.toLocaleString(Qt.locale(), "f", 0))
            }))
            onActivated: settings.apply()
        }

        QQC2.ComboBox {
            id: bufferBox
            Kirigami.FormData.label: i18nc("@label:listbox", "Buffer size:")
            textRole: "text"
            valueRole: "value"
            model: Engine.bufferSizes.map(size => ({
                value: size,
                text: i18ncp("@item:inlistbox buffer size", "%1 sample", "%1 samples", size)
            }))
            onActivated: settings.apply()
        }

        QQC2.Label {
            Kirigami.FormData.label: i18nc("@label", "Latency:")
            text: Engine.sampleRate > 0
                ? i18nc("@info milliseconds", "%1 ms", (Engine.bufferSize / Engine.sampleRate * 1000).toLocaleString(Qt.locale(), "f", 1))
                : ""
        }
    }
}
