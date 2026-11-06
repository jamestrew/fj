# fj GitHub mirror

Unofficial mirror of [rasterstate/fj](https://rasterhub.com/rasterstate/fj), maintained for access from networks that allow GitHub.

[Source code](https://github.com/jamestrew/fj/tree/main) · [Release downloads](https://github.com/jamestrew/fj/releases) · [Sync status](https://github.com/jamestrew/fj/actions/workflows/sync.yml)

The `mirror` default branch holds this notice and the sync action. Upstream branches and tags retain their original commits. Select `main` to browse the source. Report project bugs [upstream](https://rasterhub.com/rasterstate/fj/issues).

The action runs every Monday at 09:23 UTC and supports **Run workflow** in the Actions tab. It copies every public release and asset, verifies published SHA256SUMS, and compares GitHub asset hashes before uploading. It uses the built-in GitHub token and does not build or execute upstream code.

Sync updates source refs, release notes, and changed assets. Upstream deletions are retained here. The `mirror` branch is reserved for configuration. GitHub can disable scheduled workflows after 60 days without repository activity; check the sync status if updates stop.

Release assets retain their upstream bytes, including the Homebrew formula. That formula and upstream installation instructions can still reference RasterHub. Download binaries directly from this mirror when RasterHub is unavailable. Source builds can require the custom `fjord` Cargo registry.

Upstream source is MIT licensed. Its license and copyright remain on the mirrored source branches and tags.
