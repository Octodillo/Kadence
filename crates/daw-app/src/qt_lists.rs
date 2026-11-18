//! Rust collections as the Qt lists QML properties and roles take, and the
//! role plumbing the list models share.

use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QList, QListElement, QMap,
    QMapPair_QString_QVariant, QModelIndex, QString, QStringList, QVariant, QVector,
};

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

/// A list model's roles: an enum whose variants are numbered after
/// `Qt::UserRole` in declaration order, each with the name QML delegates
/// use. Declare one with [`model_roles!`](crate::model_roles), so `data()`
/// matches on the enum and the compiler checks that every role is served.
pub trait ModelRole: Copy + 'static {
    /// Every role with its QML name, in role-number order.
    const ALL: &'static [(Self, &'static str)];

    /// The role Qt asks `data()` for, if it is one of these.
    fn from_qt(role: i32) -> Option<Self> {
        let i = usize::try_from(role.checked_sub(USER_ROLE)?).ok()?;
        Self::ALL.get(i).map(|(r, _)| *r)
    }

    /// What `roleNames()` returns.
    fn names() -> QHash<QHashPair_i32_QByteArray> {
        let mut names = QHash::<QHashPair_i32_QByteArray>::default();
        for (i, (_, name)) in Self::ALL.iter().enumerate() {
            names.insert(USER_ROLE + i as i32, QByteArray::from(*name));
        }
        names
    }
}

/// One item's roles by name, as a model's `get` gives them to code that is
/// not a delegate: `value` answers each role.
pub fn role_map<R: ModelRole>(
    mut value: impl FnMut(R) -> QVariant,
) -> QMap<QMapPair_QString_QVariant> {
    let mut map = QMap::<QMapPair_QString_QVariant>::default();
    for (role, name) in R::ALL {
        map.insert(QString::from(*name), value(*role));
    }
    map
}

/// `Qt::UserRole`.
const USER_ROLE: i32 = 0x0100;

/// The Qt role numbers of `roles`, for `dataChanged`.
pub fn role_numbers<R: ModelRole + PartialEq>(roles: &[R]) -> QVector<i32> {
    let mut numbers = QVector::default();
    for role in roles {
        let i = R::ALL
            .iter()
            .position(|(r, _)| r == role)
            .expect("ModelRole::ALL lists every variant");
        numbers.append(USER_ROLE + i as i32);
    }
    numbers
}

/// The item a model index points at, if its row is in range.
pub fn row<'a, T>(items: &'a [T], index: &QModelIndex) -> Option<&'a T> {
    items.get(usize::try_from(index.row()).ok()?)
}

/// Declares a list model's role enum and its [`ModelRole`] names:
///
/// ```ignore
/// model_roles!(enum Role { Name => "name", Muted => "muted" });
/// ```
#[macro_export]
macro_rules! model_roles {
    ($(#[$meta:meta])* $vis:vis enum $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident => $qml:literal),+ $(,)?
    }) => {
        $(#[$meta])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq)]
        $vis enum $name { $($(#[$variant_meta])* $variant),+ }

        impl $crate::qt_lists::ModelRole for $name {
            const ALL: &'static [(Self, &'static str)] = &[$(($name::$variant, $qml)),+];
        }
    };
}
