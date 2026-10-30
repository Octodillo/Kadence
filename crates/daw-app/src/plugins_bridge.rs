//! `Plugins`: the QML list model of installed plugins, for the plugin
//! browser, over the app's [`PluginCatalog`](daw_session::catalog::PluginCatalog).
//!
//! Each row is a plugin that passes the current filter. The model starts a
//! background scan when QML first uses it; `tick` picks up its progress and
//! results. Plugin names, vendors and scan failures are the plugins' own
//! text; the format names (CLAP, VST3, LV2) are not translated.

use std::path::PathBuf;
use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QStringList, QUrl, QVariant,
};
use daw_core::plugin::{PluginDescriptor, PluginFormatKind};
use daw_session::catalog::{FORMATS, Filter, PluginKind};

use crate::settings::{self, KConfigStore};
use crate::state;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        // Declared once, in the Mixer's bridge; declaring it here too would
        // define its casts twice.
        include!(<QtCore/QAbstractListModel>);
        type QAbstractListModel = crate::mixer_bridge::qobject::QAbstractListModel;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        #[qml_element]
        #[qml_singleton]
        /// A scan is running.
        #[qproperty(bool, scanning)]
        /// Bundles scanned so far, and found, by the running scan; the
        /// total is 0 until the scan has looked through the folders.
        #[qproperty(i32, scan_done)]
        #[qproperty(i32, scan_total)]
        /// A scan has finished, so an empty list means none installed.
        #[qproperty(bool, has_scanned)]
        /// Rows: plugins that pass the filter.
        #[qproperty(i32, count)]
        /// Plugins installed, whatever the filter.
        #[qproperty(i32, installed_count)]
        /// Bundles the last scan could not load, and why (the plugin's or
        /// the format's own words, in English), in parallel.
        #[qproperty(QStringList, failed_bundles)]
        #[qproperty(QStringList, failure_reasons)]
        /// Folders the user added to the search.
        #[qproperty(QStringList, extra_folders)]
        type Plugins = super::PluginsRust;

        /// Picks up scan progress and results. Called by the UI's timer.
        #[qinvokable]
        fn tick(self: Pin<&mut Plugins>);

        /// Scans again, retrying plugins that failed before.
        #[qinvokable]
        fn rescan(self: Pin<&mut Plugins>);

        /// Shows plugins whose name or vendor holds every word of `text`,
        /// of `format` (0 any, 1 CLAP, 2 VST3, 3 LV2) and `kind` (0 any,
        /// 1 instruments, 2 effects).
        #[qinvokable]
        fn set_filter(self: Pin<&mut Plugins>, text: &QString, format: i32, kind: i32);

        /// Adds a folder to the search, saves it and rescans. False if it
        /// is not a local folder or already listed.
        #[qinvokable]
        fn add_folder(self: Pin<&mut Plugins>, url: &QUrl) -> bool;

        #[qinvokable]
        fn remove_folder(self: Pin<&mut Plugins>, index: i32);

        /// Row `row` as [formatId, pluginId, name], or empty.
        #[qinvokable]
        fn plugin_at(self: &Plugins, row: i32) -> QStringList;
    }

    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginResetModel"]
        fn begin_reset_model(self: Pin<&mut Plugins>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        fn end_reset_model(self: Pin<&mut Plugins>);
    }

    extern "RustQt" {
        #[qinvokable]
        #[cxx_override]
        fn data(self: &Plugins, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &Plugins) -> QHash_i32_QByteArray;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &Plugins, parent: &QModelIndex) -> i32;
    }

    impl cxx_qt::Initialize for Plugins {}
}

/// `name` and `vendor` are the plugin's; `format` is its display name,
/// `formatId` the name `Mixer.addPluginInsert` takes; `pluginId` its id.
const ROLES: &[&str] = &[
    "name",
    "vendor",
    "format",
    "formatId",
    "pluginId",
    "instrument",
];
const USER_ROLE: i32 = 0x0100;

#[derive(Default)]
pub struct PluginsRust {
    count: i32,
    scanning: bool,
    scan_done: i32,
    scan_total: i32,
    has_scanned: bool,
    installed_count: i32,
    failed_bundles: QStringList,
    failure_reasons: QStringList,
    extra_folders: QStringList,
    /// The catalog's revision when `rows` was read.
    revision: Option<u64>,
    filter: Filter,
    rows: Vec<PluginDescriptor>,
}

fn string_list(items: impl IntoIterator<Item = String>) -> QStringList {
    let mut list = QStringList::default();
    for s in items {
        list.append(QString::from(&s));
    }
    list
}

fn same_rows(a: &[PluginDescriptor], b: &[PluginDescriptor]) -> bool {
    let key = |d: &PluginDescriptor| {
        (
            d.format.clone(),
            d.id.clone(),
            d.name.clone(),
            d.vendor.clone(),
            d.is_instrument,
        )
    };
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| key(x) == key(y))
}

/// A format's name as users know it.
pub fn format_label(format: &PluginFormatKind) -> &'static str {
    match format {
        PluginFormatKind::Clap => "CLAP",
        PluginFormatKind::Vst3 => "VST3",
        PluginFormatKind::Lv2 => "LV2",
    }
}

/// The filter for the browser's choices: `format` and `kind` as in
/// `Plugins.setFilter`, out-of-range values meaning any.
pub fn filter_for(text: &str, format: i32, kind: i32) -> Filter {
    let format = usize::try_from(format)
        .ok()
        .and_then(|i| i.checked_sub(1))
        .and_then(|i| FORMATS.get(i))
        .cloned();
    let kind = match kind {
        1 => Some(PluginKind::Instrument),
        2 => Some(PluginKind::Effect),
        _ => None,
    };
    Filter {
        text: text.to_owned(),
        format,
        kind,
    }
}

impl cxx_qt::Initialize for qobject::Plugins {
    fn initialize(mut self: Pin<&mut Self>) {
        let folders = settings::load_plugin_folders(&KConfigStore);
        state::with(|s| {
            s.plugins.set_extra_folders(folders);
            if !s.plugins.has_scanned() && !s.plugins.is_scanning() {
                s.plugins.scan(false);
            }
        });
        self.as_mut().reload();
    }
}

impl qobject::Plugins {
    /// Re-reads the catalog: rows, scan state and folders.
    fn reload(mut self: Pin<&mut Self>) {
        let filter = self.filter.clone();
        let (revision, rows, installed, failed, folders, scanned) = state::with(|s| {
            let c = &s.plugins;
            (
                c.revision(),
                c.filtered(&filter).cloned().collect::<Vec<_>>(),
                c.plugins().len(),
                c.failed().to_vec(),
                c.extra_folders().to_vec(),
                c.has_scanned(),
            )
        });
        if !same_rows(&rows, &self.rows) {
            let count = i32::try_from(rows.len()).unwrap_or(i32::MAX);
            self.as_mut().begin_reset_model();
            self.as_mut().rust_mut().rows = rows;
            self.as_mut().end_reset_model();
            self.as_mut().set_count(count);
        }
        self.as_mut().rust_mut().revision = Some(revision);
        self.as_mut()
            .set_installed_count(i32::try_from(installed).unwrap_or(i32::MAX));
        self.as_mut().set_has_scanned(scanned);
        self.as_mut().set_failed_bundles(string_list(
            failed
                .iter()
                .map(|f| f.bundle.to_string_lossy().into_owned()),
        ));
        self.as_mut()
            .set_failure_reasons(string_list(failed.iter().map(|f| f.reason.clone())));
        self.as_mut().set_extra_folders(string_list(
            folders.iter().map(|f| f.to_string_lossy().into_owned()),
        ));
        self.as_mut().tick_progress();
    }

    fn tick_progress(mut self: Pin<&mut Self>) {
        let progress = state::with(|s| s.plugins.progress());
        let count = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
        self.as_mut().set_scanning(progress.is_some());
        let p = progress.unwrap_or_default();
        self.as_mut().set_scan_done(count(p.done));
        self.as_mut().set_scan_total(count(p.total));
    }

    fn tick(mut self: Pin<&mut Self>) {
        let revision = state::with(|s| {
            s.tick_plugins();
            s.plugins.revision()
        });
        if self.revision != Some(revision) {
            self.as_mut().reload();
        } else {
            self.as_mut().tick_progress();
        }
    }

    fn rescan(mut self: Pin<&mut Self>) {
        state::with(|s| s.plugins.scan(true));
        self.as_mut().reload();
    }

    fn set_filter(mut self: Pin<&mut Self>, text: &QString, format: i32, kind: i32) {
        let filter = filter_for(&text.to_string(), format, kind);
        if filter != self.filter {
            self.as_mut().rust_mut().filter = filter;
            self.as_mut().reload();
        }
    }

    fn set_folders(mut self: Pin<&mut Self>, folders: Vec<PathBuf>) {
        settings::save_plugin_folders(&mut KConfigStore, &folders);
        state::with(|s| {
            s.plugins.set_extra_folders(folders);
            s.plugins.scan(false);
        });
        self.as_mut().reload();
    }

    fn add_folder(self: Pin<&mut Self>, url: &QUrl) -> bool {
        let Some(path) = url.to_local_file() else {
            return false;
        };
        let path = PathBuf::from(path.to_string());
        let mut folders = state::with(|s| s.plugins.extra_folders().to_vec());
        // One path per line in the settings.
        if path.to_string_lossy().contains('\n') || folders.contains(&path) {
            return false;
        }
        folders.push(path);
        self.set_folders(folders);
        true
    }

    fn remove_folder(self: Pin<&mut Self>, index: i32) {
        let mut folders = state::with(|s| s.plugins.extra_folders().to_vec());
        let Some(i) = usize::try_from(index).ok().filter(|&i| i < folders.len()) else {
            return;
        };
        folders.remove(i);
        self.set_folders(folders);
    }

    fn plugin_at(&self, row: i32) -> QStringList {
        let Some(p) = usize::try_from(row).ok().and_then(|r| self.rows.get(r)) else {
            return QStringList::default();
        };
        string_list([
            daw_plugins::kind_name(&p.format).to_owned(),
            p.id.clone(),
            p.name.clone(),
        ])
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(p) = usize::try_from(index.row())
            .ok()
            .and_then(|r| self.rows.get(r))
        else {
            return QVariant::default();
        };
        let text = |s: &str| QVariant::from(&QString::from(s));
        match ROLES.get((role - USER_ROLE) as usize).copied() {
            Some("name") => text(&p.name),
            Some("vendor") => text(&p.vendor),
            Some("format") => text(format_label(&p.format)),
            Some("formatId") => text(daw_plugins::kind_name(&p.format)),
            Some("pluginId") => text(&p.id),
            Some("instrument") => QVariant::from(&p.is_instrument),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        for (i, name) in ROLES.iter().enumerate() {
            roles.insert(USER_ROLE + i as i32, QByteArray::from(*name));
        }
        roles
    }

    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        i32::try_from(self.rows.len()).unwrap_or(i32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_follow_the_browser_choices() {
        assert_eq!(filter_for("", 0, 0), Filter::default());
        let f = filter_for("eq", 3, 2);
        assert_eq!(f.text, "eq");
        assert_eq!(f.format, Some(PluginFormatKind::Lv2));
        assert_eq!(f.kind, Some(PluginKind::Effect));
        assert_eq!(filter_for("", 1, 1).format, Some(PluginFormatKind::Clap));
        assert_eq!(filter_for("", 2, 1).kind, Some(PluginKind::Instrument));
        let odd = filter_for("", 9, -1);
        assert_eq!((odd.format, odd.kind), (None, None));
    }
}
