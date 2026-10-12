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

Building and testing the whole workspace needs Python 3.11 or newer with its shared library
(`python3-dev` on Debian and Ubuntu), because of the Python host; see
[Installation](installation.md#python).

Before opening a pull request, run what CI runs:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Working on this book

The book lives in `book/` and is built with [mdBook](https://rust-lang.github.io/mdBook/)
0.4 (CI uses 0.4.52):

```sh
cargo install mdbook --version 0.4.52 --locked
mdbook serve book      # live preview at http://localhost:3000
```

Images live in `book/src/images/`, where both the book and the README use them. The studio
screenshots are rendered headlessly from the studio's tests:

```sh
RURO_SCREENSHOTS=book/src/images cargo test -p ruro-studio screenshots
```

Check every command, flag and field you document against the implementation, and keep
[Project status](status.md) honest about what is verified and what is not.

## Reporting issues

- Bugs and feature requests: [GitHub issues](https://github.com/mhussainahmad/ruro/issues).
- Security vulnerabilities: please report privately, as described in
  [SECURITY.md](https://github.com/mhussainahmad/ruro/blob/main/SECURITY.md).
