//! Host-directory share descriptor — a neutral type shared by the core
//! [`Config`](https://docs.rs/vmette) share API and the daemon's run
//! [`Request`](crate::daemon::Request). It lives here, not under `daemon`,
//! because it is not a daemon-specific concern.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A host directory exposed to the guest: `<tag>` → `<path>`. The single
/// share-descriptor type for the whole workspace (re-exported as
/// `vmette::ShareMount` for the core config API and used by
/// [`crate::daemon::Request`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareMount {
    pub tag: String,
    pub path: PathBuf,
}
