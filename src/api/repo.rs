//! Repository APIs (core CRUD, deploy keys, social actions).
//!
//! This is a thin facade. The implementation lives in flat sibling modules
//! (`repo_core`, `repo_collaborators`, `repo_keys`, `repo_social`, `repo_transfer`) to keep each file focused; the
//! glob re-exports below preserve every `api::repo::*` path so call sites
//! need no changes.

pub use crate::api::repo_collaborators::*;
pub use crate::api::repo_core::*;
pub use crate::api::repo_keys::*;
pub use crate::api::repo_social::*;
pub use crate::api::repo_transfer::*;
