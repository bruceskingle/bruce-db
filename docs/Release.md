# Releasing bruce-db

How a version of bruce-db is published to crates.io. Each version is developed on a `vX.Y.Z` branch, published from
it, tagged, and merged to `main`. A published version can never be changed or removed, only yanked, which stops new
projects from choosing it, so everything is checked before `cargo publish`.

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
cargo clippy --all-targets
cargo test
cargo deny check
cargo publish --dry-run
```

bruce-util-progress, which bruce-db uses, is published first, from its own repository, see its docs/Release.md;
until it is, the dry run fails to find it on crates.io. The dry run packages bruce-db as crates.io would get it,
builds it against the published bruce-util-progress, not the checkout next to it, and stops before uploading.
`cargo package --list` shows what is in the package.

Then commit and push, as `cargo publish` refuses a checkout with uncommitted changes, and records the commit it
was published from in the package:

```sh
git commit -am "Release 0.1.0"
git push
cargo publish
```

Check https://crates.io/crates/bruce-db, and its documentation on https://docs.rs/bruce-db, which builds within a few
minutes.

Tag the release and merge it to `main`:

```sh
git tag -a v0.1.0 -m "bruce-db 0.1.0"
git push origin v0.1.0
git switch main
git merge --squash v0.1.0 && git commit -m "Release 0.1.0"
git push
```

The next version's branch is made when work on it starts, as its number depends on what it is: a fix, e.g.
`v0.1.1`, or something new, e.g. `v0.2.0`. It starts from the release's tag, with its version in `Cargo.toml`:

```sh
git switch -c v0.1.1 v0.1.0
```

## Apps which use it

An app depends on the published version, e.g. `bruce-db = "0.1.0"`, and, to work on bruce-db alongside it, a
git-ignored `.cargo/config.toml` in the app's checkout points at the checkout of bruce-db next to it:

```toml
[patch.crates-io]
bruce-db = { path = "../bruce-db" }
bruce-util-progress = { path = "../bruce-util-progress" }
```
