# clip-core (vendored)

`clip-core` is the CLIP STUDIO PAINT and Krita document reader/writer used by
`crates/sumi-clip`. It is vendored here from the `csp-rea` repository
([github.com/mafuzyk/csp-rea](https://github.com/mafuzyk/csp-rea)) at commit
`7707a12`, under GPL-3.0-or-later, unmodified except for `Cargo.toml`: the
dependencies are stated concretely instead of inherited from the upstream
workspace, and an empty `[workspace]` table makes this copy its own workspace
root. The workspace above lists it in `exclude`, so it is a path dependency but
not a member: its sample-dependent tests and its third-party style stay out of
that workspace's test, clippy and format runs. Fixes belong upstream in
`csp-rea`; this copy is refreshed from there rather than patched in place.
