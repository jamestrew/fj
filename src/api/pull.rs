//! Pull request APIs (core CRUD/merge, reviews, commit status).
//!
//! This is a thin facade. The implementation lives in flat sibling modules
//! (`pull_core`, `pull_review`, `pull_status`) to keep each file focused; the
//! glob re-exports below preserve every `api::pull::*` path so call sites need
//! no changes.

pub use crate::api::pull_core::*;
pub use crate::api::pull_review::*;
pub use crate::api::pull_status::*;
