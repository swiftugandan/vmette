//! Pixel geometry shared between the perception layer (settle detection) and
//! the desktop wire replies.

use serde::{Deserialize, Serialize};

/// A rectangle in pixel coordinates. Used both internally by the daemon's
/// settle detector and on the wire as a moving-region / damage box in
/// [`crate::daemon::SettleReply`] and [`crate::daemon::ChangedReply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}
