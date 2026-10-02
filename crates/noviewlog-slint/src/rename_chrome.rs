//! Inline rename click-away contract (TERMINALS / FILES / filter tabs).
//!
//! Slint chrome MUST match [`click_away_dismisses`]. Mouse leave never dismisses.

/// Where a pointer event can land while an inline rename is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameHit {
    /// The active rename `TextInput` (or its chrome rectangle).
    RenameField,
    /// Unused stretch under TERMINALS/FILES/FILTERS (`sidebar-dead-space`).
    SidebarDeadSpace,
    /// Empty FILES list (0 rows → list height is 0; hits land on [`Self::SidebarDeadSpace`]).
    FilesEmptyStretch,
    /// Empty TERMINALS list (same as FILES).
    TerminalsEmptyStretch,
    Viewport,
    FindBar,
    StatusBar,
    /// Stopped-Terminal launch preview strip above the viewport.
    LaunchPreview,
    FollowChip,
    WrapChip,
    MenuBar,
    TabStripGutter,
    TabChipOther,
    /// Click on the same tab chip that is being renamed (outside the field).
    TabChipRenamingChrome,
    TerminalRowOther,
    TerminalRowRenamingChrome,
    /// Viewport V/H scrollbar track or thumb (`EngineScrollBar`).
    Scrollbar,
    FilesHeader,
    TerminalsHeader,
    SidebarPlus,
    FilterRow,
}

/// Pointer kind relevant to rename (hover/leave must not dismiss).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenamePointer {
    Down,
    Click,
    Move,
    Leave,
}

/// True when this hit MUST end the rename session (commit if name non-empty).
pub fn click_away_dismisses(hit: RenameHit, pointer: RenamePointer) -> bool {
    match pointer {
        RenamePointer::Move | RenamePointer::Leave => false,
        RenamePointer::Down | RenamePointer::Click => match hit {
            RenameHit::RenameField => false,
            RenameHit::SidebarDeadSpace
            | RenameHit::FilesEmptyStretch
            | RenameHit::TerminalsEmptyStretch
            | RenameHit::Viewport
            | RenameHit::FindBar
            | RenameHit::StatusBar
            | RenameHit::LaunchPreview
            | RenameHit::FollowChip
            | RenameHit::WrapChip
            | RenameHit::MenuBar
            | RenameHit::TabStripGutter
            | RenameHit::TabChipOther
            | RenameHit::TabChipRenamingChrome
            | RenameHit::TerminalRowOther
            | RenameHit::TerminalRowRenamingChrome
            | RenameHit::Scrollbar
            | RenameHit::FilesHeader
            | RenameHit::TerminalsHeader
            | RenameHit::SidebarPlus
            | RenameHit::FilterRow => true,
        },
    }
}

/// Layout rule: empty expanded section lists have zero height, so leftover
/// sidebar stretch is the only hit target under FILES/TERMINALS.
pub fn sidebar_list_height_px(count: i32, expanded: bool, row_px: i32, gap_px: i32) -> i32 {
    if !expanded || count <= 0 {
        return 0;
    }
    let n = count.min(5);
    n * row_px + (n - 1).max(0) * gap_px
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISMISS_HITS: &[RenameHit] = &[
        RenameHit::SidebarDeadSpace,
        RenameHit::FilesEmptyStretch,
        RenameHit::TerminalsEmptyStretch,
        RenameHit::Viewport,
        RenameHit::FindBar,
        RenameHit::StatusBar,
        RenameHit::LaunchPreview,
        RenameHit::FollowChip,
        RenameHit::WrapChip,
        RenameHit::MenuBar,
        RenameHit::TabStripGutter,
        RenameHit::TabChipOther,
        RenameHit::TabChipRenamingChrome,
        RenameHit::TerminalRowOther,
        RenameHit::TerminalRowRenamingChrome,
        RenameHit::Scrollbar,
        RenameHit::FilesHeader,
        RenameHit::TerminalsHeader,
        RenameHit::SidebarPlus,
        RenameHit::FilterRow,
    ];

    #[test]
    fn every_chrome_surface_except_the_field_dismisses_on_pointer_down() {
        for hit in DISMISS_HITS {
            assert!(
                click_away_dismisses(*hit, RenamePointer::Down),
                "{hit:?} down must dismiss"
            );
            assert!(
                click_away_dismisses(*hit, RenamePointer::Click),
                "{hit:?} click must dismiss"
            );
        }
        assert!(!click_away_dismisses(
            RenameHit::RenameField,
            RenamePointer::Down
        ));
        assert!(!click_away_dismisses(
            RenameHit::RenameField,
            RenamePointer::Click
        ));
    }

    #[test]
    fn mouse_move_and_leave_never_dismiss() {
        let all = DISMISS_HITS
            .iter()
            .copied()
            .chain(std::iter::once(RenameHit::RenameField));
        for hit in all {
            assert!(
                !click_away_dismisses(hit, RenamePointer::Move),
                "{hit:?} move must keep rename"
            );
            assert!(
                !click_away_dismisses(hit, RenamePointer::Leave),
                "{hit:?} leave must keep rename"
            );
        }
    }

    #[test]
    fn empty_files_list_is_zero_height_so_dead_space_is_the_hit_target() {
        assert_eq!(sidebar_list_height_px(0, true, 44, 3), 0);
        assert_eq!(sidebar_list_height_px(0, false, 44, 3), 0);
        assert_eq!(sidebar_list_height_px(1, true, 44, 3), 44);
        assert_eq!(sidebar_list_height_px(2, true, 44, 3), 91);
        // Regression: click "under FILES" with 0 files is SidebarDeadSpace, not the list.
        assert!(click_away_dismisses(
            RenameHit::SidebarDeadSpace,
            RenamePointer::Down
        ));
        assert!(click_away_dismisses(
            RenameHit::FilesEmptyStretch,
            RenamePointer::Down
        ));
    }

    #[test]
    fn contract_covers_every_hit_variant() {
        // Fail the build if a new surface is added without a dismiss decision.
        let variants = [
            RenameHit::RenameField,
            RenameHit::SidebarDeadSpace,
            RenameHit::FilesEmptyStretch,
            RenameHit::TerminalsEmptyStretch,
            RenameHit::Viewport,
            RenameHit::FindBar,
            RenameHit::StatusBar,
            RenameHit::LaunchPreview,
            RenameHit::FollowChip,
            RenameHit::WrapChip,
            RenameHit::MenuBar,
            RenameHit::TabStripGutter,
            RenameHit::TabChipOther,
            RenameHit::TabChipRenamingChrome,
            RenameHit::TerminalRowOther,
            RenameHit::TerminalRowRenamingChrome,
            RenameHit::Scrollbar,
            RenameHit::FilesHeader,
            RenameHit::TerminalsHeader,
            RenameHit::SidebarPlus,
            RenameHit::FilterRow,
        ];
        assert_eq!(variants.len(), 21);
        for hit in variants {
            let _ = click_away_dismisses(hit, RenamePointer::Down);
            let _ = click_away_dismisses(hit, RenamePointer::Leave);
        }
    }
}
