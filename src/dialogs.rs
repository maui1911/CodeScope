//! App-level modal dialogs. Each sub-module is a single self-contained
//! dialog the [`crate::app::AppShell`] composes into its z-stack.

pub mod confirm;
pub mod new_project;
pub mod new_worktree;
pub mod rename;
pub mod settings;
