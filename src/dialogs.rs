//! App-level modal dialogs. Each sub-module is a single self-contained
//! dialog the [`crate::app::AppShell`] composes into its z-stack.

pub mod add_board;
pub mod confirm;
pub mod decide;
pub mod new_project;
pub mod new_worktree;
pub mod pick_project;
pub mod project_settings;
pub mod rename;
pub mod settings;
