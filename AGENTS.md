# Notes for Coding Agents

bruce-db is a Rust crate, a record store with a hash key, a sort key and a version on every record, files now and
DynamoDB later, and replication between nodes. The design is [docs/design.md](docs/design.md); change it there when
the design changes, as it is the master copy.

## Building

- Std only, no async runtime: it must build for ESP-IDF targets as well as macOS, Linux and Windows, so no
  dependency may need an OS feature ESP-IDF lacks.
- `cargo build` and `cargo test`. Format with `cargo fmt`, see `rustfmt.toml`; `cargo fmt --check` should pass.
- It is published to crates.io, so a published version can never be changed or removed. Do not publish; the
  maintainer does.

## Conventions

- **IDs** are made only by the crate's ID code, which enforces the rules in the design: strings normalised to NFC, no
  arrays, numbers as IEEE doubles.
- **Errors** are formatted with `{error:#}`, their chain of causes, not `{error:?}`.
- **Comments** are doc comments in full sentences, saying what a thing is for and why.
- Never put an account's details, keys or passwords in a test, a commit or a log; tests use made-up data.
