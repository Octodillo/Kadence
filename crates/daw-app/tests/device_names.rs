//! Every name the built-in devices show (device, parameter and choice
//! names) has a translation in `qml/DeviceNames.qml`, and the table has
//! none that no device uses.

use std::collections::BTreeSet;

fn catalog_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for e in daw_devices::devices() {
        names.insert(e.info.name.to_owned());
        for p in e.params {
            names.insert(p.name.to_owned());
            if let daw_devices::ParamKind::Choice(labels) = p.kind {
                names.extend(labels.iter().map(|l| (*l).to_owned()));
            }
        }
    }
    names
}

/// The keys of the table: `"Key": i18nc(` lines.
fn table_names() -> BTreeSet<String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/qml/DeviceNames.qml");
    let qml = std::fs::read_to_string(path).expect("DeviceNames.qml");
    qml.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let rest = l.strip_prefix('"')?;
            let (key, after) = rest.split_once('"')?;
            after.starts_with(": i18nc(").then(|| key.to_owned())
        })
        .collect()
}

#[test]
fn every_device_name_has_a_translation() {
    let catalog = catalog_names();
    let table = table_names();
    let missing: Vec<_> = catalog.difference(&table).collect();
    let unused: Vec<_> = table.difference(&catalog).collect();
    assert!(
        missing.is_empty() && unused.is_empty(),
        "DeviceNames.qml is missing {missing:?} and has unused {unused:?}"
    );
}
