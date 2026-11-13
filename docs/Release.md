# Releasing bruce-db

How a version of bruce-db is published to crates.io. A published version can never be changed or removed, only yanked,
which stops new projects from choosing it, so everything is checked before `cargo publish`.

Releases are made with [bruce-method-release](https://github.com/bruceskingle/bruce-method-release), run in this
checkout: `bruce-method-release check` runs every check, changing nothing, and `bruce-method-release release` does
the steps below, in order, after listing them and asking. The release's notes go in `docs/ChangeLog.md` first, under
`# Release X.Y.Z`, which the check requires. The steps by hand, which the tool follows, are kept here for reference.

## Names

- **`vX.Y.Z`**: the branch a version is developed on, with `X.Y.Z` in `Cargo.toml`.
- **`releaseX.Y.Z`**: the annotated tag on `main` of the commit published as `X.Y.Z`.
- **`main`**: only releases, each one a squash merge of its branch.

After each release the next version's branch, `vX.(Y+1).0`, is started from `main`. A fix to a released version is
made on a `vX.Y.(Z+1)` branch from its tag, only when one is needed.

## Once: a crates.io account and token

1. Sign in at https://crates.io with GitHub, as bruceskingle.
2. In Account Settings, add and verify an email address, without which crates.io refuses to publish.
3. In Account Settings, API Tokens, make a new token for this computer: scopes `publish-new` and `publish-update`,
   crates `bruce-*`, and an expiry, e.g. a year. It is shown once.
4. `cargo login`, and paste the token when asked. Cargo keeps it in `~/.cargo/credentials.toml`, a secret file like the
   app's settings: never print it.

## Each release

The checks, which CI runs too, see `.github/workflows/ci.yml`, except clippy and the dry run, then push:

```sh
cd ~/git/bruce-db
git switch v0.1.0
git status --short
cargo fmt --all --check
cargo clippy --all-targets
cargo test
cargo deny check
cargo publish --dry-run
git push
```

The dry run packages bruce-db as crates.io would get it, builds it against the published versions of its dependencies,
e.g. bruce-util-progress, not checkouts next to it, and stops before uploading; a dependency with changes it needs is
released first. `cargo package --list` shows what is in the package.

Squash merge the branch to `main` with a pull request, tag `main`, and publish from the tag, so that the tag, `main`
and the published crate are the same commit, which crates.io records in the package's `.cargo_vcs_info.json`:

```sh
gh pr create --base main --head v0.1.0 --title "V0.1.0" --body "Release 0.1.0"
gh pr merge v0.1.0 --squash
git fetch origin
git tag -a release0.1.0 origin/main -m "bruce-db release0.1.0"
git push origin release0.1.0
git switch --detach release0.1.0
cargo publish
```

Check https://crates.io/crates/bruce-db, and its documentation on https://docs.rs/bruce-db, which builds within a few
minutes.

Then start the next version's branch from `main`, with its version in `Cargo.toml`:

```sh
git switch -c v0.2.0 origin/main
# version = "0.2.0" in Cargo.toml
cargo build
git commit -am "Start v0.2.0"
git push -u origin v0.2.0
```

## A fix to a released version

On a branch from the release's tag, released as above, its pull request squash merged to `main`, which holds the last
release, then merged into the branch being developed, so that it has the fix too:

```sh
git switch -c v0.1.1 release0.1.0
# version = "0.1.1" in Cargo.toml, the fix, then the release as above
git switch v0.2.0
git merge origin/main
```

## 0.1.0

0.1.0 was published from the branch on 10 November 2026, before this order was settled, and tagged on 11 November.
`main` was fast forwarded to the tag, rather than squash merged, so that the tag, `main` and the published crate
are the same commit, as after any other release.

## Apps which use it

An app depends on the published version, e.g. `bruce-db = "0.1.0"`, and, to work on bruce-db alongside it, a
git-ignored `.cargo/config.toml` in the app's checkout points at the checkout of bruce-db next to it, which is taken
out again once the changes made with it are released:

```toml
[patch.crates-io]
bruce-db = { path = "../bruce-db" }
```
