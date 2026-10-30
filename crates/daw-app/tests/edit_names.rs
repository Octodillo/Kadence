//! Every edit name the model can put in the undo history has a translation
//! in `qml/EditNames.qml`.
//!
//! The names are English string literals spread over the crates. This finds
//! them where commands are named and fails when one is missing from the QML
//! table, or when the table has one no command uses, so a new command cannot
//! slip into the Undo menu untranslated.

use std::path::{Path, PathBuf};

/// Calls whose first string literal argument is an edit name.
const NAMING_CALLS: &[&str] = &[
    "Group::new(",
    "Batch::new(",
    "NoteEdit::new(",
    "edit_selected(",
    "edit_selection(",
    "edit_selected_audio(",
    "relink(",
];

/// How many lines after a naming call to look for its name, for calls
/// formatted over several lines.
const CALL_LINES: usize = 4;

/// String literals in `text`.
fn literals(text: &str) -> Vec<&str> {
    text.split('"').skip(1).step_by(2).collect()
}

/// The code before the unit test module, which names commands of its own.
fn without_tests(source: &str) -> &str {
    source.split("#[cfg(test)]\nmod ").next().unwrap_or(source)
}

/// Every `.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn edit_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        // `fn label(&self) -> String { ... }` and `let label = if ... {}`:
        // every literal up to the end of the block, which may be this line.
        if line.contains("fn label(&self)") || line.trim_start().starts_with("let label = ") {
            names.extend(literals(line).into_iter().map(str::to_owned));
            let trimmed = line.trim_end();
            if trimmed.ends_with(';') || (trimmed.ends_with('}') && trimmed.contains('{')) {
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            for body in &lines[i + 1..] {
                let body_indent = body.len() - body.trim_start().len();
                if body_indent <= indent && matches!(body.trim(), "}" | "};") {
                    break;
                }
                names.extend(literals(body).into_iter().map(str::to_owned));
            }
            continue;
        }
        for call in NAMING_CALLS {
            if let Some(at) = line.find(call) {
                let end = (i + CALL_LINES).min(lines.len());
                let text = std::iter::once(&line[at..])
                    .chain(lines[i + 1..end].iter().copied())
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Some(name) = literals(&text).first() {
                    names.push((*name).to_owned());
                }
            }
        }
    }
    names
}

/// The English names `EditNames.qml` has entries for.
fn qml_keys(qml: &str) -> Vec<String> {
    qml.lines()
        .filter(|l| l.contains("i18nc(\"@item:inmenu name of an edit\""))
        .filter_map(|l| literals(l).first().map(|k| (*k).to_owned()))
        .collect()
}

#[test]
fn every_edit_name_is_translated() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let qml =
        std::fs::read_to_string(manifest.join("qml/EditNames.qml")).expect("read EditNames.qml");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(manifest.join("..")).expect("read crates") {
        rust_files(&entry.expect("crate").path().join("src"), &mut files);
    }
    let mut found = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        found.extend(edit_names(without_tests(&source)));
    }
    found.sort();
    found.dedup();
    // A sanity check that the scan still finds names at all.
    assert!(found.len() > 40, "found only {found:?}");
    let missing: Vec<&String> = found
        .iter()
        .filter(|name| {
            let entry = format!("\"{name}\": i18nc(\"@item:inmenu name of an edit\", \"{name}\")");
            !qml.contains(&entry)
        })
        .collect();
    assert!(
        missing.is_empty(),
        "edit names without a translation in qml/EditNames.qml: {missing:?}"
    );
    let stale: Vec<String> = qml_keys(&qml)
        .into_iter()
        .filter(|key| !found.contains(key))
        .collect();
    assert!(
        stale.is_empty(),
        "qml/EditNames.qml translates names no command uses: {stale:?}"
    );
}

#[test]
fn scan_finds_names_in_every_layout() {
    let source = r#"
        fn label(&self) -> String { "One Line".into() }
        fn label(&self) -> String {
            match self { A => "Block", B => "Other" }.into()
        }
        let label = "Let Line";
        Group::new(
            "Split Call",
            items,
        );
        edit_selected(doc, env, "Same Line", |c| c);
    "#;
    let mut names = edit_names(source);
    names.sort();
    assert_eq!(
        names,
        [
            "Block",
            "Let Line",
            "One Line",
            "Other",
            "Same Line",
            "Split Call"
        ]
    );
}
