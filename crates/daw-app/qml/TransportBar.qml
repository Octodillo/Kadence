// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Above the arrange view: where the playhead is (bar.beat as the engine
// counts it, and time), the tempo and time signature in effect there, and
// the metronome. Every tempo and meter change is an undoable Project edit.
QQC2.ToolBar {
    id: bar

    position: QQC2.ToolBar.Header

    // Opens the metronome settings; the self test uses it for a screenshot.
    function openClickSettings(): void {
        clickPopup.open();
    }

    function closeClickSettings(): void {
        clickPopup.close();
    }

    // Beat note values the time signature offers.
    readonly property list<int> denominators: [1, 2, 4, 8, 16, 32]

    // Tap tempo: the average of the gaps between recent taps.
    property list<real> taps: []

    function tap(): void {
        const now = Date.now();
        // A pause longer than the slowest tempo starts over.
        const kept = taps.filter(t => now - t < 60000 / 20);
        kept.push(now);
        taps = kept.slice(-8);
        if (taps.length >= 2) {
            const gap = (taps[taps.length - 1] - taps[0]) / (taps.length - 1);
            Project.setTempo(Math.round(60000 / gap * 10) / 10);
        }
    }

    // The controls set their own values when used, which replaces a plain
    // binding; follow the project (undo, moving the playhead) from here.
    Connections {
        target: Project
        function onBpmChanged() { tempoField.value = Math.round(Project.bpm * tempoField.scale); }
        function onMeterNumeratorChanged() { numeratorField.value = Project.meterNumerator; }
        function onMeterDenominatorChanged() { denominatorField.currentIndex = bar.denominators.indexOf(Project.meterDenominator); }
        function onMetronomeVolumeChanged() { levelSlider.value = Project.metronomeVolume; }
        function onCountInBarsChanged() { countInField.currentIndex = Project.countInBars; }
        function onMetronomeChanged() { metronomeButton.checked = Project.metronome; }
    }

    contentItem: Flow {
        spacing: Kirigami.Units.largeSpacing

        // Bar.beat and time. During a count-in the playhead waits, so say so.
        RowLayout {
            spacing: Kirigami.Units.smallSpacing
            height: tempoField.implicitHeight

            QQC2.Label {
                Layout.minimumWidth: Kirigami.Units.gridUnit * 4
                text: Project.countingIn ? i18nc("@info:status the metronome counts in before recording", "Count-in")
                                         : Project.positionText
                font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.4
                font.features: { "tnum": 1 }
                Accessible.name: i18nc("@info:accessible", "Playhead position in bars and beats")
            }
            QQC2.Label {
                text: Project.timeText
                font.features: { "tnum": 1 }
                opacity: 0.7
                Accessible.name: i18nc("@info:accessible", "Playhead position in minutes and seconds")
            }
        }

        RowLayout {
            spacing: Kirigami.Units.smallSpacing
            height: tempoField.implicitHeight

            QQC2.Label {
                text: i18nc("@label:spinbox", "Tempo:")
            }

            // The value is hundredths of a beat per minute: SpinBox is
            // integer only.
            QQC2.SpinBox {
                id: tempoField

                readonly property int scale: 100

                from: 20 * scale
                to: 999 * scale
                stepSize: scale
                value: Math.round(Project.bpm * scale)
                editable: true
                wheelEnabled: true
                Layout.preferredWidth: Kirigami.Units.gridUnit * 6
                textFromValue: (v, locale) => Number(v / scale).toLocaleString(locale, "f", v % scale === 0 ? 0 : 2)
                valueFromText: (text, locale) => Math.round(Number.fromLocaleString(locale, text) * scale)
                validator: DoubleValidator {
                    bottom: 20
                    top: 999
                    decimals: 2
                    notation: DoubleValidator.StandardNotation
                }
                onValueModified: Project.setTempo(value / scale)

                QQC2.ToolTip.text: i18nc("@info:tooltip", "Tempo in beats per minute at the playhead. Scroll or drag up and down to change it.")
                QQC2.ToolTip.visible: hovered && !dragger.active
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                Accessible.name: i18nc("@info:accessible", "Tempo in beats per minute")

                // Drag up for faster: one beat per minute every few pixels.
                DragHandler {
                    id: dragger
                    target: null
                    xAxis.enabled: false
                    property real startValue: 0
                    onActiveChanged: if (active) startValue = Project.bpm
                    onTranslationChanged: {
                        if (active) {
                            const steps = Math.round(-translation.y / (Kirigami.Units.gridUnit / 3));
                            Project.setTempo(Math.max(20, Math.min(999, startValue + steps)));
                        }
                    }
                }
            }

            QQC2.Label {
                text: i18nc("@label unit after the tempo, beats per minute", "BPM")
                opacity: 0.7
            }

            QQC2.ToolButton {
                text: i18nc("@action:button tap tempo", "Tap")
                display: QQC2.AbstractButton.TextOnly
                onClicked: bar.tap()
                QQC2.ToolTip.text: i18nc("@info:tooltip", "Tap along to set the tempo")
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            }
        }

        RowLayout {
            spacing: Kirigami.Units.smallSpacing
            height: tempoField.implicitHeight

            QQC2.Label {
                text: i18nc("@label:spinbox", "Time signature:")
            }
            QQC2.SpinBox {
                id: numeratorField
                from: 1
                to: 32
                value: Project.meterNumerator
                editable: true
                wheelEnabled: true
                Layout.preferredWidth: Kirigami.Units.gridUnit * 4
                onValueModified: Project.setTimeSignature(value, Project.meterDenominator)
                Accessible.name: i18nc("@info:accessible", "Beats per bar")
            }
            QQC2.Label {
                text: "/"
            }
            QQC2.ComboBox {
                id: denominatorField
                model: bar.denominators
                currentIndex: bar.denominators.indexOf(Project.meterDenominator)
                onActivated: (index) => Project.setTimeSignature(Project.meterNumerator, bar.denominators[index])
                Accessible.name: i18nc("@info:accessible", "Beat note value")
            }
        }

        RowLayout {
            spacing: 0
            height: tempoField.implicitHeight

            QQC2.ToolButton {
                id: metronomeButton
                icon.name: "chronometer"
                text: i18nc("@action:button", "Metronome")
                display: QQC2.AbstractButton.IconOnly
                checkable: true
                checked: Project.metronome
                onToggled: Project.setMetronomeEnabled(checked)
                QQC2.ToolTip.text: i18nc("@info:tooltip", "Metronome: click on every beat while playing")
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            }
            QQC2.ToolButton {
                id: clickMenuButton
                icon.name: "arrow-down"
                display: QQC2.AbstractButton.IconOnly
                text: i18nc("@action:button", "Metronome Settings")
                checked: clickPopup.visible
                onClicked: clickPopup.visible ? clickPopup.close() : clickPopup.open()
                QQC2.ToolTip.text: text
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay

                QQC2.Popup {
                    id: clickPopup
                    // Right-aligned under the button, inside the window.
                    x: clickMenuButton.width - width
                    y: clickMenuButton.height
                    margins: Kirigami.Units.smallSpacing
                    // A press on the button closes it through the button,
                    // rather than closing it and opening it again.
                    closePolicy: QQC2.Popup.CloseOnEscape | QQC2.Popup.CloseOnPressOutsideParent
                    padding: Kirigami.Units.largeSpacing

                    contentItem: Kirigami.FormLayout {
                        QQC2.Slider {
                            id: levelSlider
                            Kirigami.FormData.label: i18nc("@label:slider", "Level:")
                            from: -40
                            to: 6
                            value: Project.metronomeVolume
                            implicitWidth: Kirigami.Units.gridUnit * 10
                            // Saved once the handle is let go, not on every move.
                            onMoved: Project.setMetronomeLevel(Math.round(value * 2) / 2, !pressed)
                            onPressedChanged: if (!pressed) Project.setMetronomeLevel(Math.round(value * 2) / 2, true)
                            Accessible.name: i18nc("@info:accessible", "Metronome level")

                            QQC2.ToolTip.text: i18nc("@info:tooltip %1 is a level in decibels", "%1 dB", Number(value).toLocaleString(Qt.locale(), "f", 1))
                            QQC2.ToolTip.visible: hovered || pressed
                        }
                        QQC2.ComboBox {
                            id: countInField
                            Kirigami.FormData.label: i18nc("@label:listbox", "Count-in before recording:")
                            model: [
                                i18nc("@item:inlistbox no count-in", "None"),
                                i18ncp("@item:inlistbox count-in length", "%1 bar", "%1 bars", 1),
                                i18ncp("@item:inlistbox count-in length", "%1 bar", "%1 bars", 2)
                            ]
                            currentIndex: Project.countInBars
                            onActivated: (index) => Project.setCountIn(index)
                        }
                    }
                }
            }
        }
    }
}
