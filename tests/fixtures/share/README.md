# tests/fixtures/share

Stable host directory used as the `--share` target in smoke tests.
Mount inside the guest is `/mnt/host`. Files starting with `from-guest`
are written by the guest during a test run and are gitignored.

Replaces the earlier `/tmp/vz-share-test` ad-hoc directory.
