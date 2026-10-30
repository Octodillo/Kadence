// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Dialogs
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

Kirigami.ApplicationWindow {
    id: root

    // Set from C++ (see kadence.cpp).
    required property var aboutData
    required property bool smokeTest
    required property bool benchmark
    required property string screenshotDir

    // A project that was never named or saved has an empty name. This is %1
    // in the window titles below.
    readonly property string projectName: Project.name || i18nc("@title:window name of a new, unsaved project", "Untitled")

    title: Project.modified ? i18nc("@title:window %1 is the project name", "%1 [modified] — Kadence", projectName)
                            : i18nc("@title:window %1 is the project name", "%1 — Kadence", projectName)
    width: Kirigami.Units.gridUnit * 55
    height: Kirigami.Units.gridUnit * 35
    minimumWidth: Kirigami.Units.gridUnit * 22
    minimumHeight: Kirigami.Units.gridUnit * 18

    readonly property list<Kirigami.Action> pageActions: [
        Kirigami.Action {
            id: arrangeAction
            text: i18nc("@action:inmenu", "Arrange")
            icon.name: "view-media-track"
            shortcut: "Ctrl+1"
            checkable: true
            checked: true
            onTriggered: root.showPage(arrangePage, arrangeAction)
        },
        Kirigami.Action {
            id: mixAction
            text: i18nc("@action:inmenu", "Mix")
            icon.name: "view-media-equalizer"
            shortcut: "Ctrl+2"
            checkable: true
            onTriggered: root.showPage(mixPage, mixAction)
        },
        Kirigami.Action {
            id: editAction
            text: i18nc("@action:inmenu", "Edit")
            icon.name: "editor"
            shortcut: "Ctrl+3"
            checkable: true
            onTriggered: root.showPage(editPage, editAction)
        }
    ]

    // True when the user tabbed to a button (not clicked it), so Space
    // should press that button rather than start playback.
    readonly property bool buttonHasKeyboardFocus: activeFocusItem instanceof QQC2.AbstractButton
                                                   && activeFocusItem.visualFocus

    // Shared by every page's toolbar.
    readonly property Kirigami.Action playAction: Kirigami.Action {
        text: Project.playing ? i18nc("@action:button", "Stop") : i18nc("@action:button", "Play")
        icon.name: Project.playing ? "media-playback-stop" : "media-playback-start"
        // A window shortcut gets Space before the focused control does, so
        // step aside while a button has keyboard focus and let Space press it.
        shortcut: root.buttonHasKeyboardFocus ? "" : "Space"
        displayHint: Kirigami.DisplayHint.KeepVisible
        onTriggered: Project.togglePlay()
    }

    readonly property Kirigami.Action recordAction: Kirigami.Action {
        text: Project.recording ? i18nc("@action:button", "Stop Recording") : i18nc("@action:button", "Record")
        icon.name: "media-record"
        shortcut: "R"
        tooltip: i18nc("@info:tooltip", "Record onto the armed tracks; with none armed, record MIDI into the open clip or onto the selected instrument track")
        displayHint: Kirigami.DisplayHint.KeepVisible
        onTriggered: Project.toggleRecord()
    }

    Kirigami.Action {
        id: goToStartAction
        text: i18nc("@action", "Go to Start")
        icon.name: "media-skip-backward"
        shortcut: "Home"
        onTriggered: Project.goToStart()
    }

    readonly property list<Kirigami.Action> fileActions: [
        Kirigami.Action {
            text: i18nc("@action:inmenu", "New Project")
            icon.name: "document-new"
            shortcut: StandardKey.New
            onTriggered: Project.newProject()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Open Project…")
            icon.name: "document-open"
            shortcut: StandardKey.Open
            onTriggered: openDialog.open()
        },
        Kirigami.Action {
            // A project is a folder. A file dialog can't pick one whose name
            // ends in .kadence (the filter makes it look like a file), and
            // without the project file inside it there's nothing to pick,
            // so offer the folder itself too.
            text: i18nc("@action:inmenu", "Open Project Folder…")
            icon.name: "folder-open"
            onTriggered: openFolderDialog.open()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Save")
            icon.name: "document-save"
            shortcut: StandardKey.Save
            onTriggered: root.save()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Save As…")
            icon.name: "document-save-as"
            shortcut: StandardKey.SaveAs
            onTriggered: saveDialog.open()
        }
    ]

    readonly property list<Kirigami.Action> historyActions: [
        Kirigami.Action {
            text: Project.canUndo ? i18nc("@action:inmenu %1 is an edit, like Move Clip", "Undo %1", editNames.name(Project.undoText))
                                  : i18nc("@action:inmenu", "Undo")
            icon.name: "edit-undo"
            shortcut: StandardKey.Undo
            enabled: Project.canUndo
            onTriggered: Project.undo()
        },
        Kirigami.Action {
            text: Project.canRedo ? i18nc("@action:inmenu %1 is an edit, like Move Clip", "Redo %1", editNames.name(Project.redoText))
                                  : i18nc("@action:inmenu", "Redo")
            icon.name: "edit-redo"
            shortcut: StandardKey.Redo
            enabled: Project.canRedo
            onTriggered: Project.redo()
        }
    ]

    function save(): void {
        if (!Project.savedBefore) {
            saveDialog.open();
            return;
        }
        notify(Project.save());
    }

    FileDialog {
        id: openDialog
        title: i18nc("@title:window", "Open Project")
        nameFilters: [i18nc("@item:inlistbox", "Kadence projects (*.kadence)")]
        onAccepted: {
            root.notify(Project.openProject(selectedFile));
        }
    }

    FolderDialog {
        id: openFolderDialog
        title: i18nc("@title:window", "Open Project Folder")
        onAccepted: {
            root.notify(Project.openProject(selectedFolder));
        }
    }

    FileDialog {
        id: saveDialog
        title: i18nc("@title:window", "Save Project As")
        fileMode: FileDialog.SaveFile
        nameFilters: [i18nc("@item:inlistbox", "Kadence projects (*.kadence)")]
        onAccepted: {
            root.notify(Project.saveAs(selectedFile));
        }
    }

    // Engine position, peak loading, autosave.
    Timer {
        interval: 33
        repeat: true
        running: true
        onTriggered: Project.tick()
    }

    Kirigami.Action {
        id: separatorAction
        separator: true
    }

    readonly property Kirigami.Action settingsAction: Kirigami.Action {
        text: i18nc("@action:inmenu", "Configure Kadence…")
        icon.name: "configure"
        shortcut: StandardKey.Preferences
        onTriggered: root.openSettings()
    }

    readonly property Kirigami.Action aboutAction: Kirigami.Action {
        text: i18nc("@action:inmenu", "About Kadence")
        icon.name: "help-about"
        onTriggered: root.openAbout()
    }

    readonly property Kirigami.Action quitAction: Kirigami.Action {
        text: i18nc("@action:inmenu", "Quit")
        icon.name: "application-exit"
        shortcut: StandardKey.Quit
        onTriggered: Qt.quit()
    }

    globalDrawer: Kirigami.GlobalDrawer {
        title: i18nc("@title", "Kadence")
        titleIcon: "io.github.octodillo.daw"
        modal: !root.wideScreen
        collapsible: true
        showHeaderWhenCollapsed: true
        actions: [
            root.fileActions[0],
            root.fileActions[1],
            root.fileActions[2],
            root.fileActions[3],
            root.fileActions[4],
            separatorAction,
            root.historyActions[0],
            root.historyActions[1],
            goToStartAction,
            separatorAction,
            arrangeAction,
            mixAction,
            editAction,
            separatorAction,
            root.settingsAction,
            root.aboutAction,
            root.quitAction
        ]
    }

    pageStack.initialPage: arrangePage
    pageStack.globalToolBar.style: Kirigami.ApplicationHeaderStyle.ToolBar
    pageStack.columnView.columnResizeMode: Kirigami.ColumnView.SingleColumn

    // The status line is on the Arrange page; elsewhere (recording from
    // the Mix or Edit page, a mixer error) a new message pops up instead.
    Connections {
        target: Project
        function onStatusChanged() {
            if (root.pageStack.currentItem !== arrangePage) {
                root.notify(Project.status);
            }
        }
    }

    ArrangePage {
        id: arrangePage
        visible: false
        transportActions: [root.playAction, root.recordAction]
    }
    MixPage {
        id: mixPage
        visible: false
        transportActions: [root.playAction, root.recordAction]
    }
    EditPage {
        id: editPage
        visible: false
        transportActions: [root.playAction, root.recordAction]
    }

    // Double-clicking a clip in the arrange view opens it in the editor.
    Connections {
        target: arrangePage.view
        function onClipActivated() { root.showPage(editPage, editAction); }
    }

    Component {
        id: aboutComponent
        Kirigami.AboutPage {
            objectName: "aboutPage"
            aboutData: root.aboutData
        }
    }

    EngineErrorText {
        id: engineError
    }

    ProjectMessageText {
        id: projectMessage
    }

    EditNames {
        id: editNames
    }

    // Shows a message map from Project, if it holds one.
    function notify(message: var): void {
        const text = projectMessage.text(message);
        if (text !== "") {
            showPassiveNotification(text, "long");
        }
    }

    // Audio trouble the user did not cause from the settings page (those
    // errors show there): say so wherever they are, with a way to fix it.
    function notifyEngineError(): void {
        switch (Engine.errorContext) {
        case Engine.StartupFallback:
        case Engine.StartupFailed:
        case Engine.AudioFailed:
            showPassiveNotification(engineError.text, "long",
                                    i18nc("@action:button", "Audio Settings…"),
                                    () => root.openSettings());
            break;
        }
    }

    Component.onCompleted: notifyEngineError()

    Connections {
        target: Engine
        function onErrorContextChanged(): void {
            root.notifyEngineError();
        }
    }

    function showPage(page: Kirigami.Page, action: Kirigami.Action): void {
        for (const a of pageActions) {
            a.checked = a === action;
        }
        if (pageStack.currentItem !== page) {
            pageStack.replace(page);
        }
    }

    function openSettings(): Item {
        return pageStack.pushDialogLayer(Qt.resolvedUrl("SettingsPage.qml"), {}, {
            title: i18nc("@title:window", "Configure — Kadence"),
            width: Kirigami.Units.gridUnit * 30,
            height: Kirigami.Units.gridUnit * 22
        });
    }

    function openAbout(): Item {
        // The drawer stays clickable next to the layer on a wide window, so
        // don't stack a second About page on the first.
        // instanceof does not match QML-defined Kirigami types, so compare
        // the objectName set on aboutComponent instead.
        if (pageStack.layers.currentItem?.objectName === "aboutPage") {
            return pageStack.layers.currentItem;
        }
        return pageStack.layers.push(aboutComponent);
    }

    // `--smoke-test` and `--screenshot-dir`: walk through the app, check the
    // Rust bridge, optionally save screenshots, then quit. Used by CI.
    Loader {
        active: root.smokeTest || root.screenshotDir !== ""
        sourceComponent: SelfTest {
            window: root
        }
    }

    // `--benchmark`: frame times while scrolling a large project.
    Loader {
        active: root.benchmark
        sourceComponent: Benchmark {
            window: root
        }
    }
}
