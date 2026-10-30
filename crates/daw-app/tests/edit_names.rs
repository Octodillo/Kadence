//! Every edit name the model can put in the undo history has a translation
//! in `qml/EditNames.qml`.
//!
//! The names are English string literals spread over the model, arrange,
//! MIDI and piano roll crates. This finds them where commands are named and
//! fails when one is missing from the QML table, so a new command cannot
//! slip into the Undo menu untranslated.

use std::path::Path;

/// Source files that name commands, relative to `crates/`.
const SOURCES: &[&str] = &[
    "daw-model/src/commands.rs",
    "daw-arrange/src/commands.rs",
    "daw-arrange/src/lib.rs",
    "daw-midi/src/model.rs",
    "daw-pianoroll/src/lib.rs",
];

/// Calls whose first string literal argument is an edit name.
const NAMING_CALLS: &[&str] = &[
    "Group::new(",
    "Batch::new(",
    "NoteEdit::new(",
    "edit_selected(",
    "edit_selection(",
];

/// String literals in `text`.
fn literals(text: &str) -> Vec<&str> {
    text.split('"').skip(1).step_by(2).collect()
}

/// The code before the unit tests, which name commands of their own.
fn without_tests(source: &str) -> &str {
    source.split("#[cfg(test)]").next().unwrap_or(source)
}

fn edit_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        // `fn label(&self) -> String { ... }` and `let label = if ... {}`:
        // every literal up to the end of the block.
        if line.contains("fn label(&self)") || line.trim_start().starts_with("let label = ") {
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
            if let Some(at) = line.find(call)
                && let Some(name) = literals(&line[at..]).first()
            {
                names.push((*name).to_owned());
            }
        }
    }
    names
}

#[test]
fn every_edit_name_is_translated() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let qml =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("qml/EditNames.qml"))
            .expect("read EditNames.qml");
    let mut found = Vec::new();
    for file in SOURCES {
        let source = std::fs::read_to_string(crates.join(file))
            .unwrap_or_else(|e| panic!("read {file}: {e}"));
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
}
