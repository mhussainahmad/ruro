# Contributing to ruro

Thanks for your interest in ruro. Bug reports, documentation fixes, new nodes and features are
all welcome. This document explains how to set up a development environment and what we look
for in a pull request.

## Getting help and discussing ideas

- **Bugs and feature requests:** [GitHub issues](https://github.com/mhussainahmad/ruro/issues).
  Please use the templates.
- **Security issues:** do not open a public issue; see [SECURITY.md](SECURITY.md).
- **Larger changes:** open an issue describing the problem and your proposed approach before
  writing a lot of code, so we can agree on the design first.

<!-- TODO(maintainers): add links to community channels (chat, forum) once they exist. -->

## Development setup

You need Linux, a recent stable Rust toolchain from [rustup](https://rustup.rs) (the studio's
GUI dependencies need Rust 1.95 or newer) and the windowing development libraries for the
studio. On Debian and Ubuntu:

```sh
sudo apt-get install libxkbcommon-dev libwayland-dev libgl1-mesa-dev
```

Then:

```sh
git clone https://github.com/mhussainahmad/ruro
cd ruro
cargo build
cargo test --workspace
```

Try your changes end to end with the examples:

```sh
cargo run -p ruro-cli -- run examples/camera.yaml
cargo run -p ruro-studio -- examples/camera.yaml
```

Set `RURO_LOG=debug` (or a filter such as `info,ruro_host=debug`) for more logging.

## Before you open a pull request

CI runs the following; please run them locally first:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- Add tests for new behaviour and bug fixes. Unit tests live next to the code; the studio has
  headless UI tests in `crates/ruro-studio/src/app/tests.rs`.
- Update the documentation when behaviour changes: the [README](README.md), the book in
  `book/` (build it with `mdbook build book` or preview with `mdbook serve book`) and
  [CHANGELOG.md](CHANGELOG.md) under `[Unreleased]`.
- If you change the studio's appearance, re-render the screenshots headlessly:

  ```sh
  RURO_SCREENSHOTS=docs/images cargo test -p ruro-studio screenshots
  ```

- If you touch performance-sensitive code (ports, queues, transport), compare benchmarks
  before and after:

  ```sh
  cargo bench -p ruro-node -p ruro-transport
  ```

## Code style

- Format with `rustfmt` defaults; no clippy warnings.
- Prefer small, focused crates and functions; keep the node API (`ruro`, `ruro-node`,
  `ruro-msg`) minimal and stable.
- Public items get doc comments. Node types and parameters get doc comments too: they are
  shown in the studio.
- Errors: `anyhow` in binaries and node code, `thiserror` for library error types.
- No `unsafe` without a `// SAFETY:` comment explaining why it is sound.

## Commits and pull requests

- Write commit messages in the imperative mood with a short summary line (about 72 characters
  at most), a blank line, then an explanation of what and why if it is not obvious:

  ```text
  Restart groups with exponential backoff

  A group that crashed in a loop was restarted every 200 ms forever.
  Back off up to 5 s and reset after the group has been stable for 10 s.
  ```

- Keep pull requests focused on one change. Refactors and behaviour changes are easier to
  review separately.
- Fill in the pull request template, link the issue it addresses, and mention anything you
  could not test.

## Licensing

ruro is dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE). Unless you
explicitly state otherwise, any contribution intentionally submitted for inclusion in ruro by
you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.

## Code of conduct

Everyone participating in this project is expected to follow the
[Code of Conduct](CODE_OF_CONDUCT.md).
