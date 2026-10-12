# Contributing

Contributions are welcome: bug reports, documentation, new nodes and features. The full guide
is [CONTRIBUTING.md](https://github.com/mhussainahmad/ruro/blob/main/CONTRIBUTING.md) in the
repository; the essentials:

```sh
git clone https://github.com/mhussainahmad/ruro
cd ruro
cargo build
cargo test --workspace
```

Before opening a pull request, run what CI runs:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Working on this book

The book lives in `book/` and is built with [mdBook](https://rust-lang.github.io/mdBook/):

```sh
cargo install mdbook --locked
mkdir -p book/src/images && cp docs/images/*.png book/src/images/
mdbook serve book      # live preview at http://localhost:3000
```

Screenshots come from `docs/images/` and are rendered headlessly from the studio:

```sh
RURO_SCREENSHOTS=docs/images cargo test -p ruro-studio screenshots
```

Pages for features that are still being built live under `book/src/upcoming/` and start with a
status note. When a feature ships, move its page into the main sections and check every
command and field against the implementation.

## Reporting issues

- Bugs and feature requests: [GitHub issues](https://github.com/mhussainahmad/ruro/issues).
- Security vulnerabilities: please report privately, as described in
  [SECURITY.md](https://github.com/mhussainahmad/ruro/blob/main/SECURITY.md).
