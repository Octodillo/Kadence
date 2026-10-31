//! Rust collections as the Qt lists QML properties and roles take.

use cxx_qt_lib::{QList, QListElement, QString, QStringList};

pub fn string_list<S: AsRef<str>>(items: impl IntoIterator<Item = S>) -> QStringList {
    let mut list = QStringList::default();
    for s in items {
        list.append(QString::from(s.as_ref()));
    }
    list
}

pub fn list<T: QListElement + cxx::ExternType<Kind = cxx::kind::Trivial>>(
    items: impl IntoIterator<Item = T>,
) -> QList<T> {
    let mut list = QList::default();
    for item in items {
        list.append(item);
    }
    list
}
