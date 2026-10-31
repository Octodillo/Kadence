// SPDX-License-Identifier: GPL-3.0-or-later

import QtQml
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
            onTriggered: root.whenChangesHandled(() => Project.newProject())
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Open Project…")
            icon.name: "document-open"
            shortcut: StandardKey.Open
            onTriggered: openDialog.open()
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
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Export Song…")
            icon.name: "document-export"
            shortcut: "Ctrl+Shift+E"
            onTriggered: root.openExport()
        }
    ]

    // Filled from Project.recentProjects by recentInstantiator.
    readonly property Kirigami.Action openRecentAction: Kirigami.Action {
        text: i18nc("@action:inmenu", "Open Recent")
        icon.name: "document-open-recent"
        enabled: Project.recentProjects.length > 0
    }

    Instantiator {
        id: recentInstantiator
        model: Project.recentProjects
        delegate: Kirigami.Action {
            required property int index
            required property string modelData
            text: modelData.split("/").pop()
            tooltip: modelData
            icon.name: "folder-sound"
            onTriggered: root.openRecent(index)
        }
        onObjectAdded: rebuild()
        onObjectRemoved: rebuild()
        function rebuild(): void {
            const list = [];
            for (let i = 0; i < count; ++i) {
                if (objectAt(i)) {
                    list.push(objectAt(i));
                }
            }
            root.openRecentAction.children = list;
        }
    }

    readonly property Kirigami.Action collectAction: Kirigami.Action {
        text: i18nc("@action:inmenu", "Collect Audio Files")
        icon.name: "folder-download"
        tooltip: i18nc("@info:tooltip", "Copy the audio files the project uses from elsewhere into its folder, so it can be moved or shared")
        enabled: Project.savedBefore && Project.externalCount > 0
        onTriggered: {
            const n = Project.collectFiles();
            if (n > 0) {
                showPassiveNotification(i18ncp("@info", "Copied %1 audio file into the project.",
                                               "Copied %1 audio files into the project.", n));
            }
        }
    }

    readonly property Kirigami.Action findMissingAction: Kirigami.Action {
        text: i18nc("@action:inmenu", "Find Missing Audio Files…")
        icon.name: "edit-find"
        visible: Project.missingCount > 0
        onTriggered: relinkDialog.open()
    }

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
        currentFolder: AppSettings.projectFolderUrl
        nameFilters: [i18nc("@item:inlistbox", "Kadence projects (*.kadence)")]
        onAccepted: {
            const url = selectedFile;
            root.whenChangesHandled(() => root.opened(Project.openProject(url), url));
        }
    }

    FileDialog {
        id: saveDialog
        title: i18nc("@title:window", "Save Project As")
        currentFolder: AppSettings.projectFolderUrl
        fileMode: FileDialog.SaveFile
        nameFilters: [i18nc("@item:inlistbox", "Kadence projects (*.kadence)")]
        // Runs after a successful save, when saving was asked for before
        // closing or replacing the project.
        property var then: null
        onAccepted: {
            const result = Project.saveAs(selectedFile);
            root.notify(result);
            const next = then;
            then = null;
            if (result.context === undefined && next) {
                next();
            }
        }
        onRejected: then = null
    }

    // Runs `proceed` once the project's unsaved changes are saved or the
    // user chose to discard them; not at all if they cancel.
    function whenChangesHandled(proceed: var): void {
        if (!Project.modified) {
            proceed();
            return;
        }
        unsavedDialog.proceed = proceed;
        unsavedDialog.open();
    }

    Kirigami.PromptDialog {
        id: unsavedDialog
        property var proceed: null
        title: i18nc("@title:window", "Save Changes?")
        subtitle: i18nc("@info %1 is the project name",
                        "The project “%1” has been modified. Do you want to save your changes or discard them?",
                        root.projectName)
        standardButtons: Kirigami.Dialog.Save | Kirigami.Dialog.Discard | Kirigami.Dialog.Cancel
        onAccepted: {
            const next = proceed;
            proceed = null;
            if (!Project.savedBefore) {
                saveDialog.then = next;
                saveDialog.open();
                return;
            }
            const result = Project.save();
            root.notify(result);
            if (result.context === undefined) {
                next();
            }
        }
        onDiscarded: {
            const next = proceed;
            proceed = null;
            close();
            next();
        }
        onRejected: proceed = null
    }

    // After opening a project: offer to recover unsaved changes it holds,
    // say why it failed, or offer to find missing audio files.
    function opened(result: var, url: url): void {
        if (result.context === Project.OpenContext && result.reason === Project.AutosavePending) {
            autosaveDialog.url = url;
            autosaveDialog.open();
            return;
        }
        notify(result);
        if (result.context === undefined && Project.missingCount > 0) {
            relinkDialog.open();
        }
    }

    function openRecent(index: int): void {
        const url = Project.recentUrl(index);
        root.whenChangesHandled(() => root.opened(Project.openRecent(url), url));
    }

    Kirigami.PromptDialog {
        id: autosaveDialog
        property url url
        title: i18nc("@title:window", "Recover Unsaved Changes?")
        subtitle: i18nc("@info",
                        "This project has changes from a session that ended without saving. Recover them, or open the last saved version and discard them?")
        standardButtons: Kirigami.Dialog.Cancel
        customFooterActions: [
            Kirigami.Action {
                text: i18nc("@action:button", "Recover")
                icon.name: "document-revert"
                onTriggered: {
                    autosaveDialog.close();
                    root.opened(Project.recoverProject(autosaveDialog.url), autosaveDialog.url);
                }
            },
            Kirigami.Action {
                text: i18nc("@action:button", "Open Saved Version")
                icon.name: "edit-delete"
                onTriggered: {
                    autosaveDialog.close();
                    root.opened(Project.openDiscardingAutosave(autosaveDialog.url), autosaveDialog.url);
                }
            }
        ]
    }

    RelinkDialog {
        id: relinkDialog
        onRelinked: (count) => {
            if (count > 0 && Project.missingCount > 0) {
                root.showPassiveNotification(i18ncp("@info", "Found %1 file.", "Found %1 files.", count));
            }
        }
    }

    RecoveryDialog {
        id: recoveryDialog
        onRecovered: (result) => {
            root.notify(result);
            if (result.context === undefined && Project.missingCount > 0) {
                relinkDialog.open();
            }
        }
    }

    // For the self test.
    readonly property alias relinkDialog: relinkDialog
    readonly property alias recoveryDialog: recoveryDialog
    readonly property alias unsavedDialog: unsavedDialog

    // Set once the user agreed to close with the project as it is.
    property bool closeConfirmed: false

    onClosing: (close) => {
        if (!closeConfirmed && Project.modified && !smokeTest && screenshotDir === "") {
            close.accepted = false;
            whenChangesHandled(() => {
                root.closeConfirmed = true;
                root.close();
            });
            return;
        }
        Project.shutdown();
    }

    // Engine position, peak loading, autosave, plugin scan progress.
    Timer {
        interval: 33
        repeat: true
        running: true
        onTriggered: {
            Project.tick();
            Plugins.tick();
        }
    }

    // One browser for every place that adds a plugin; see browsePlugins().
    PluginBrowser {
        id: pluginBrowser
        property var pickHandler: null
        onChosen: (format, pluginId, name) => {
            if (pickHandler) {
                pickHandler(format, pluginId, name);
            }
        }
        onClosed: pickHandler = null
        onConfigureRequested: root.openSettings()
    }

    // MIDI devices plugged in or out.
    Timer {
        interval: 1000
        repeat: true
        running: true
        onTriggered: AppSettings.refreshMidi()
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
        // Through the window, so unsaved changes are asked about.
        onTriggered: root.close()
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
            root.openRecentAction,
            root.fileActions[2],
            root.fileActions[3],
            root.fileActions[4],
            root.collectAction,
            root.findMissingAction,
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
        onRecentActivated: (index) => root.openRecent(index)
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

    ExportDialog {
        id: exportDialog
        onExportFinished: message => root.notify(message)
    }

    function openExport(): ExportDialog {
        exportDialog.open();
        return exportDialog;
    }

    ProjectMessageText {
        id: projectMessage
    }

    EditNames {
        id: editNames
    }

    // Opened by DevicePanel.open() from any page.
    DevicePanelDrawer {
        id: devicePanel
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

    Component.onCompleted: {
        PluginEditors.setTitles(
            // The placeholders stay for the window titles to fill in.
            i18nc("@title:window a plugin's own editor; %1 is the device, %2 its track", "%1 — %2", "%1", "%2"),
            i18nc("@title name of the master bus", "Master"));
        notifyEngineError();
        // The self test must not touch the user's recent list or autosaves.
        if (!smokeTest && screenshotDir === "") {
            Project.startSession();
            if (Project.recoveryCount > 0) {
                recoveryDialog.open();
            }
        }
    }

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

    // Opens the plugin browser on `kind` (0 any, 1 instruments, 2 effects);
    // `pick(format, pluginId, name)` runs when the user chooses one.
    function browsePlugins(kind: int, pick: var): var {
        pluginBrowser.kind = kind;
        pluginBrowser.pickHandler = pick;
        pluginBrowser.open();
        return pluginBrowser;
    }

    function openSettings(): Item {
        return pageStack.pushDialogLayer(Qt.resolvedUrl("SettingsPage.qml"), {}, {
            title: i18nc("@title:window", "Configure — Kadence"),
            width: Kirigami.Units.gridUnit * 32,
            height: Kirigami.Units.gridUnit * 24
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
}
