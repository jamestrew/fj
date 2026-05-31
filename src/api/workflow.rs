//! Forgejo Actions APIs (runs, artifacts, run view/logs, secrets, variables).
//!
//! This is a thin facade. The implementation lives in flat sibling modules
//! (`workflow_run`, `workflow_view`, `workflow_secrets`) to keep each file
//! focused; the glob re-exports below preserve every `api::workflow::*` path
//! so call sites need no changes.

pub use crate::api::workflow_run::*;
pub use crate::api::workflow_secrets::*;
pub use crate::api::workflow_view::*;
