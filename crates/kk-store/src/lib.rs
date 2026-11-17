//! Workspace storage: the folder layout and the SQLite event store.

pub mod db;
pub mod workspace;

pub use db::Store;
pub use workspace::{resolve_in, WorkspaceDir};
