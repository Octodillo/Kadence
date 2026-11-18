//! `Plugins`: the QML list model of installed plugins, for the plugin
//! browser, over the app's [`PluginCatalog`](daw_session::catalog::PluginCatalog).
//!
//! Each row is a plugin that passes the current filter. The model starts a
//! background scan when QML first uses it; `tick` picks up its progress and
//! results. Plugin names, vendors and scan failures are the plugins' own
//! text; the format names (`formats`) are not translated.

use std::path::{Path, PathBuf};
use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{
    QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QStringList, QUrl, QVariant,
};
use daw_core::plugin::{PluginDescriptor, PluginFormatKind};
use daw_session::catalog::{FORMATS, Filter, PluginKind};

use crate::model_roles;
use crate::qt_lists::{ModelRole, row, string_list};
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

    /// Which plugins a browser shows.
    #[qenum(Plugins)]
    enum PluginKind {
        AnyPlugin,
        InstrumentPlugin,
        EffectPlugin,
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
        /// The formats' names as users know them, in the order a browser
        /// lists them; `setFilter` and `formatFolders` number them from 1.
        /// They never change.
        #[qproperty(QStringList, formats, READ = formats, CONSTANT)]
        type Plugins = super::PluginsRust;

        fn formats(self: &Plugins) -> QStringList;

        /// Picks up scan progress and results. Called by the UI's clock.
        #[qinvokable]
        fn tick(self: Pin<&mut Plugins>);

        /// Scans again, retrying plugins that failed before.
        #[qinvokable]
        fn rescan(self: Pin<&mut Plugins>);

        /// Shows plugins whose name or vendor holds every word of `text`,
        /// of `format` (0 any, else a format of `formats` counted from 1)
        /// and of `kind`.
        #[qinvokable]
        fn set_filter(self: Pin<&mut Plugins>, text: &QString, format: i32, kind: PluginKind);

        /// Adds a folder to the search, saves it and rescans. False if it
        /// is not a local folder or already listed.
        #[qinvokable]
        fn add_folder(self: Pin<&mut Plugins>, url: &QUrl) -> bool;

        #[qinvokable]
        fn remove_folder(self: Pin<&mut Plugins>, index: i32);

        /// The folders searched for a format (numbered as in `setFilter`,
        /// from 1) before the added ones, in search order, with the home
        /// folder written `~`. Folders outside the home folder that do
        /// not exist are left out.
        #[qinvokable]
        fn format_folders(self: &Plugins, format: i32) -> QStringList;

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

model_roles!(
    /// `name` and `vendor` are the plugin's; `format` is its display name,
    /// `formatId` the name `Mixer.addPluginInsert` takes; `pluginId` its id.
    enum Role {
        Name => "name",
        Vendor => "vendor",
        Format => "format",
        FormatId => "formatId",
        PluginId => "pluginId",
        Instrument => "instrument",
    }
);

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

/// The format `Plugins.setFilter` numbers `format`: [`FORMATS`] from 1.
fn format_at(format: i32) -> Option<PluginFormatKind> {
    usize::try_from(format)
        .ok()
        .and_then(|i| i.checked_sub(1))
        .and_then(|i| FORMATS.get(i))
        .cloned()
}

/// `$HOME` as a folder of the user's own: not when unset, empty or `/`,
/// where everything would count as theirs.
fn home_folder(home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    home.map(PathBuf::from).filter(|h| h.parent().is_some())
}

/// `path` for showing, with `home` shortened to `~` as users write it.
fn display_folder(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The searched `folders` worth showing, as text: those that `exists`, and
/// those in `home` even when they don't, since the user may create them.
/// A system folder this distribution doesn't have is only noise.
fn shown_folders(
    folders: &[PathBuf],
    home: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> Vec<String> {
    folders
        .iter()
        .filter(|f| home.is_some_and(|home| f.starts_with(home)) || exists(f))
        .map(|f| display_folder(f, home))
        .collect()
}

/// The filter for the browser's choices: `format` and `kind` as in
/// `Plugins.setFilter`, out-of-range values meaning any.
pub fn filter_for(text: &str, format: i32, kind: qobject::PluginKind) -> Filter {
    let format = format_at(format);
    let kind = match kind {
        qobject::PluginKind::InstrumentPlugin => Some(PluginKind::Instrument),
        qobject::PluginKind::EffectPlugin => Some(PluginKind::Effect),
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

    fn set_filter(
        mut self: Pin<&mut Self>,
        text: &QString,
        format: i32,
        kind: qobject::PluginKind,
    ) {
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

    fn format_folders(&self, format: i32) -> QStringList {
        let Some(format) = format_at(format) else {
            return QStringList::default();
        };
        let home = home_folder(std::env::var_os("HOME"));
        let folders = state::with(|s| s.plugins.format_folders(&format));
        string_list(shown_folders(&folders, home.as_deref(), Path::is_dir))
    }

    fn formats(&self) -> QStringList {
        string_list(FORMATS.iter().map(|f| f.display_name().to_owned()))
    }

    fn plugin_at(&self, row: i32) -> QStringList {
        let Some(p) = usize::try_from(row).ok().and_then(|r| self.rows.get(r)) else {
            return QStringList::default();
        };
        string_list([p.format.as_str().to_owned(), p.id.clone(), p.name.clone()])
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(p) = row(&self.rows, index) else {
            return QVariant::default();
        };
        let text = |s: &str| QVariant::from(&QString::from(s));
        let Some(role) = Role::from_qt(role) else {
            return QVariant::default();
        };
        match role {
            Role::Name => text(&p.name),
            Role::Vendor => text(&p.vendor),
            Role::Format => text(p.format.display_name()),
            Role::FormatId => text(p.format.as_str()),
            Role::PluginId => text(&p.id),
            Role::Instrument => QVariant::from(&p.is_instrument),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        Role::names()
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
        use qobject::PluginKind as Qml;
        assert_eq!(filter_for("", 0, Qml::AnyPlugin), Filter::default());
        let f = filter_for("eq", 3, Qml::EffectPlugin);
        assert_eq!(f.text, "eq");
        assert_eq!(f.format, Some(PluginFormatKind::Lv2));
        assert_eq!(f.kind, Some(PluginKind::Effect));
        let f = filter_for("", 1, Qml::InstrumentPlugin);
        assert_eq!(f.format, Some(PluginFormatKind::Clap));
        assert_eq!(f.kind, Some(PluginKind::Instrument));
        let odd = filter_for("", 9, Qml { repr: -1 });
        assert_eq!((odd.format, odd.kind), (None, None));
    }

    #[test]
    fn folders_show_the_home_folder_as_a_tilde() {
        let home = Some(Path::new("/home/me"));
        assert_eq!(display_folder(Path::new("/home/me/.clap"), home), "~/.clap");
        assert_eq!(display_folder(Path::new("/home/me"), home), "~");
        assert_eq!(
            display_folder(Path::new("/home/meg/.clap"), home),
            "/home/meg/.clap"
        );
        assert_eq!(
            display_folder(Path::new("/usr/lib/clap"), home),
            "/usr/lib/clap"
        );
        assert_eq!(
            display_folder(Path::new("/home/me/.clap"), None),
            "/home/me/.clap"
        );
    }

    #[test]
    fn the_root_folder_is_nobodys_home() {
        assert_eq!(
            home_folder(Some("/home/me".into())),
            Some(PathBuf::from("/home/me"))
        );
        for none in [Some("/".into()), Some("".into()), None] {
            assert_eq!(home_folder(none), None);
        }
    }

    #[test]
    fn missing_folders_are_shown_only_in_the_home_folder() {
        let folders = ["/home/me/.lv2", "/usr/lib/lv2", "/usr/lib64/lv2"].map(PathBuf::from);
        let exists = |p: &Path| p == Path::new("/usr/lib/lv2");
        assert_eq!(
            shown_folders(&folders, Some(Path::new("/home/me")), exists),
            ["~/.lv2", "/usr/lib/lv2"]
        );
        assert_eq!(shown_folders(&folders, None, exists), ["/usr/lib/lv2"]);
    }

    #[test]
    fn formats_are_numbered_from_one() {
        assert_eq!(format_at(1), Some(PluginFormatKind::Clap));
        assert_eq!(format_at(3), Some(PluginFormatKind::Lv2));
        for none in [0, 4, -1, i32::MIN] {
            assert_eq!(format_at(none), None);
        }
    }
}
