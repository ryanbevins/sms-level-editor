## Summary

- What changed?
- Why is it needed?

## Verification

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --locked --workspace --all-targets -- -D warnings`
- [ ] `cargo test --locked --workspace`
- [ ] `cargo build --locked --release -p graffito-editor`
- [ ] Runtime visual behavior was checked when applicable, or is explicitly
      listed as awaiting manual verification

## Data safety

- [ ] No retail assets, extracted archives, disc images, caches, or generated
      editor projects are included
- [ ] Base game files are never modified
