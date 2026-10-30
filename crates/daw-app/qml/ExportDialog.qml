// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Dialogs
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Renders the song to an audio file. The export runs in the background;
// the dialog shows its progress and can stop it.
Kirigami.Dialog {
    id: dialog

    // The export ended, or could not start: `message` is a message map
    // (see ProjectMessageText.qml) to show.
    signal exportFinished(var message)

    readonly property list<int> sampleRates: [44100, 48000, 88200, 96000]
    // WAV 16-bit and FLAC 16-bit; dither only helps there.
    readonly property bool sixteenBit: formatBox.currentIndex === 0 || formatBox.currentIndex === 3

    title: i18nc("@title:window", "Export Song")
    preferredWidth: Kirigami.Units.gridUnit * 24
    padding: Kirigami.Units.largeSpacing
    showCloseButton: !Exporter.running
    closePolicy: Exporter.running ? QQC2.Popup.NoAutoClose
                                  : QQC2.Popup.CloseOnEscape | QQC2.Popup.CloseOnPressOutside

    onAboutToShow: {
        Exporter.refresh();
        if (!Exporter.hasLoop && rangeBox.currentIndex === Exporter.LoopRange) {
            rangeBox.currentIndex = Exporter.WholeSong;
        }
        firstBar.value = 1;
        lastBar.value = Math.max(1, Exporter.songBars);
        const rate = sampleRates.indexOf(Exporter.engineRate);
        rateBox.currentIndex = rate >= 0 ? rate : 1;
    }

    function start(url: url): void {
        const message = Exporter.start(url, rangeBox.currentIndex, firstBar.value, lastBar.value,
                                       tailBox.value, formatBox.currentIndex,
                                       sampleRates[rateBox.currentIndex],
                                       sixteenBit && ditherBox.checked);
        if (message.context !== undefined) {
            exportFinished(message);
        }
    }

    Kirigami.FormLayout {
        enabled: !Exporter.running

        QQC2.ComboBox {
            id: rangeBox
            Kirigami.FormData.label: i18nc("@label:listbox", "Range:")
            // In ExportRange order.
            model: [
                i18nc("@item:inlistbox export range", "Whole song"),
                i18nc("@item:inlistbox export range", "Loop"),
                i18nc("@item:inlistbox export range", "Bars")
            ]
        }
        QQC2.Label {
            visible: rangeBox.currentIndex === Exporter.WholeSong
            text: Exporter.songBars > 0
                  ? i18ncp("@info", "To the end of the last clip: %1 bar",
                           "To the end of the last clip: %1 bars", Exporter.songBars)
                  : i18nc("@info", "The song has no clips yet.")
            wrapMode: Text.Wrap
            Layout.maximumWidth: Kirigami.Units.gridUnit * 16
        }
        QQC2.Label {
            visible: rangeBox.currentIndex === Exporter.LoopRange && !Exporter.hasLoop
            text: i18nc("@info", "No loop is set.")
        }
        RowLayout {
            visible: rangeBox.currentIndex === Exporter.Bars
            Kirigami.FormData.label: i18nc("@label:spinbox", "Bars:")
            spacing: Kirigami.Units.smallSpacing
            QQC2.SpinBox {
                id: firstBar
                from: 1
                to: 9999
                editable: true
                Accessible.name: i18nc("@label:spinbox", "First bar")
            }
            QQC2.Label {
                text: i18nc("@label between the first and last bar numbers", "to")
            }
            QQC2.SpinBox {
                id: lastBar
                from: firstBar.value
                to: 9999
                editable: true
                Accessible.name: i18nc("@label:spinbox", "Last bar")
            }
        }
        QQC2.SpinBox {
            id: tailBox
            Kirigami.FormData.label: i18nc("@label:spinbox", "Tail:")
            from: 0
            to: 30
            value: 2
            editable: true
            textFromValue: (value, locale) => i18ncp("@item:valuesuffix", "%1 second", "%1 seconds", value)
            valueFromText: (text, locale) => parseInt(text) || 0
            QQC2.ToolTip.text: i18nc("@info:tooltip", "Time after the end for effects and notes to ring out")
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }

        Kirigami.Separator {
            Kirigami.FormData.isSection: true
        }

        QQC2.ComboBox {
            id: formatBox
            Kirigami.FormData.label: i18nc("@label:listbox", "Format:")
            currentIndex: 1
            // In the order of Exporter.start's `format`.
            model: [
                i18nc("@item:inlistbox file format", "WAV, 16-bit"),
                i18nc("@item:inlistbox file format", "WAV, 24-bit"),
                i18nc("@item:inlistbox file format", "WAV, 32-bit float"),
                i18nc("@item:inlistbox file format", "FLAC, 16-bit"),
                i18nc("@item:inlistbox file format", "FLAC, 24-bit")
            ]
        }
        QQC2.ComboBox {
            id: rateBox
            Kirigami.FormData.label: i18nc("@label:listbox", "Sample rate:")
            model: dialog.sampleRates.map(rate => i18nc("@item:inlistbox %1 is a number", "%1 Hz",
                                                        rate.toLocaleString(Qt.locale(), "f", 0)))
        }
        QQC2.CheckBox {
            id: ditherBox
            text: i18nc("@option:check", "Dither")
            checked: true
            enabled: dialog.sixteenBit
            QQC2.ToolTip.text: i18nc("@info:tooltip", "Add a little noise when reducing to 16 bits, so quiet passages stay smooth")
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }

        QQC2.ProgressBar {
            visible: Exporter.running
            Kirigami.FormData.label: i18nc("@label", "Exporting:")
            from: 0
            to: 1
            value: Exporter.progress
            Layout.fillWidth: true
        }
    }

    customFooterActions: [
        Kirigami.Action {
            text: i18nc("@action:button", "Export…")
            icon.name: "document-export"
            enabled: !Exporter.running
            onTriggered: fileDialog.open()
        },
        Kirigami.Action {
            text: Exporter.running ? i18nc("@action:button", "Stop Export") : i18nc("@action:button", "Close")
            icon.name: Exporter.running ? "process-stop" : "dialog-close"
            onTriggered: Exporter.running ? Exporter.cancel() : dialog.close()
        }
    ]

    FileDialog {
        id: fileDialog
        title: i18nc("@title:window", "Export Song As")
        fileMode: FileDialog.SaveFile
        defaultSuffix: Exporter.extension(formatBox.currentIndex)
        nameFilters: formatBox.currentIndex < 3
                     ? [i18nc("@item:inlistbox", "WAV audio (*.wav)")]
                     : [i18nc("@item:inlistbox", "FLAC audio (*.flac)")]
        onAccepted: dialog.start(selectedFile)
    }

    Timer {
        interval: 100
        repeat: true
        running: Exporter.running
        onTriggered: Exporter.poll()
    }

    Connections {
        target: Exporter
        function onFinished(message: var): void {
            if (message.reason === Project.Exported) {
                dialog.close();
            }
            dialog.exportFinished(message);
        }
    }
}
