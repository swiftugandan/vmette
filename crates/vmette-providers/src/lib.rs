//! The **default rootfs-provider registry** for vmette, assembled once so every
//! consumer resolves `--rootfs`/`--image` specs identically.
//!
//! Provider resolution is first-match-wins, so the order here is load-bearing
//! and must have a single home: previously the CLI and the daemon each built
//! this list by hand, and a reordering in one place silently diverged from the
//! other. Both now call [`default_registry`].

use vmette::provider::{DirProvider, Registry};
use vmette_provider_oci::OciProvider;
use vmette_provider_squashfs::SquashfsProvider;
use vmette_provider_tar::TarProvider;

/// Build the standard provider [`Registry`] in resolution order.
///
/// Order matters — first-match-wins. [`DirProvider`] claims path-like specs,
/// [`SquashfsProvider`] and [`TarProvider`] claim their `<fs>+`/`tar+` schemes,
/// and [`OciProvider`] is the catch-all for everything else (bare image refs,
/// `oci://`).
pub fn default_registry() -> Registry {
    Registry::new()
        .with(DirProvider::new())
        .with(SquashfsProvider::new())
        .with(TarProvider::new())
        .with(OciProvider::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order is the contract — lock it so an accidental reshuffle (which
    /// would change which provider claims an ambiguous spec) fails the build.
    #[test]
    fn registry_order_is_stable() {
        assert_eq!(
            default_registry().names(),
            vec!["dir", "squashfs", "tar", "oci"]
        );
    }
}
