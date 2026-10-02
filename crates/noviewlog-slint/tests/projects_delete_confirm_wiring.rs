//! Issue #82: the projects dialog must not call project-delete directly from
//! the row icon — a confirm step gates the engine call.

use std::fs;
use std::path::PathBuf;

#[test]
fn project_delete_goes_through_confirm_step() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/projects-dialog.slint");
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let icon_idx = src.find("kind: \"close\";").expect("delete icon");
    let chunk = &src[icon_idx..icon_idx.saturating_add(300).min(src.len())];
    assert!(
        chunk.contains("begin-delete(proj.id"),
        "row close icon must arm the confirm step, not call project-delete"
    );
    assert!(
        !chunk.contains("root.project-delete("),
        "row close icon must not delete directly"
    );
    // The only project-delete call site is the confirm handler.
    let confirm_idx = src.find("confirm-delete\") {").expect("confirm branch");
    let tail = &src[confirm_idx..confirm_idx.saturating_add(300).min(src.len())];
    assert!(
        tail.contains("root.project-delete(root.delete-id)"),
        "confirm step must be the delete call site"
    );
}
