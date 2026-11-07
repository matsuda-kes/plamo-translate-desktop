# Stable Rust compatibility patch

Source: crates.io `gpui-pre-macos` 0.3.11, Zed revision
`f816e1f9263e3a8ef63c4baaa02e1441ce1c6038` (Apache-2.0).

The three `Option::map_or_default` calls in `src/window.rs` are replaced
with `map_or_else(Default::default, ...)`, preserving lazy default evaluation
on stable Rust. All other upstream source is unchanged.

The workspace selects this copy through `[patch.crates-io]`. Remove the
patch when the pinned GPUI dependency includes an upstream fix.
