# Contributing

## Development workflow

Create a short-lived branch from `main`, keep each change focused, and open a
pull request when the local checks pass. Do not commit extracted game data,
retail assets, generated projects, disc images, or cache output.

Run the same checks used by CI before requesting review:

```powershell
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo build --locked --release -p graffito-editor
```

Format changes must preserve unsupported data byte-for-byte. Rendering changes
must be grounded in J3D/GX behavior or the matching SMS decompilation source.
Runtime visual behavior still requires manual verification in the release
editor; successful automated checks do not establish visual correctness.
