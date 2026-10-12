## What and why

<!-- What does this change do, and why? Link the issue it addresses (e.g. "Fixes #123"). -->

## How it was tested

<!-- Commands you ran, graphs you tried, hardware or simulators involved.
     Mention anything you could not test. -->

## Checklist

- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Tests added or updated for new behaviour and bug fixes
- [ ] Documentation updated (README, `book/`, doc comments) if behaviour changed
- [ ] `CHANGELOG.md` updated under `[Unreleased]` for user-visible changes
- [ ] Studio screenshots re-rendered if the UI changed
      (`RURO_SCREENSHOTS=book/src/images cargo test -p ruro-studio screenshots`)
