# Releasing bruce-db

How a version of bruce-db and bruce-util-progress is published to crates.io. Each version is developed on a `vX.Y.Z`
branch, published from it, tagged, and merged to `main`. A published version can never be changed or removed, only
yanked, which stops new projects from choosing it, so everything is checked before `cargo publish`.

## Once: a crates.io account and token

1. Sign in at https://crates.io with GitHub, as bruceskingle.
2. In Account Settings, add and verify an email address, without which crates.io refuses to publish.
3. In Account Settings, API Tokens, make a new token for this computer: scopes `publish-new` and `publish-update`,
   crates `bruce-*`, and an expiry, e.g. a year. It is shown once.
4. `cargo login`, and paste the token when asked. Cargo keeps it in `~/.cargo/credentials.toml`, a secret file like the
   app's settings: never print it.

## Each release

The checks, all of which CI runs too, see `.github/workflows/ci.yml`, except clippy and the dry run:

```sh
cd ~/git/bruce-db
git switch v0.1.0
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
cargo deny check
cargo publish --dry-run --workspace
```

The dry run packages both crates as crates.io would get them, builds bruce-db against the packaged
bruce-util-progress, and stops before uploading. `cargo package --list -p bruce-db` shows what is in a package.

Then commit and push, as `cargo publish` refuses a checkout with uncommitted changes, and records the commit it
was published from in the package:

```sh
git commit -am "Release 0.1.0"
git push
cargo publish --workspace
```

`--workspace` publishes bruce-util-progress first, as bruce-db needs it, then bruce-db, waiting for each to be in
crates.io's index. A crate whose version is unchanged since its last release is left out with
`--exclude`, e.g. `cargo publish --workspace --exclude bruce-util-progress`, or published alone with `-p`.

Check https://crates.io/crates/bruce-db and https://crates.io/crates/bruce-util-progress, and their documentation on
docs.rs, https://docs.rs/bruce-db, which builds within a few minutes.

Tag the release, merge it to `main`, and start the next version's branch, with the next version in both crates'
`Cargo.toml`:

```sh
git tag -a v0.1.0 -m "bruce-db 0.1.0"
git push origin v0.1.0
git switch main
git merge --squash v0.1.0 && git commit -m "Release 0.1.0"
git push
git switch -c v0.1.1
```

## Apps which use it

An app depends on the published version, e.g. `bruce-db = "0.1.0"`, and, to work on bruce-db alongside it, a
git-ignored `.cargo/config.toml` in the app's checkout points at the checkout of bruce-db next to it:

```toml
[patch.crates-io]
bruce-db = { path = "../bruce-db" }
bruce-util-progress = { path = "../bruce-db/bruce-util-progress" }
```
