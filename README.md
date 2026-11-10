# bruce-db

Bruce's Database: a small NoSQL record store, in Rust, for apps which keep their data on a computer and on ESP32
devices with SD cards, and replicate it between them.

Every record has a hash key, which names its DataSet, a sort key, which orders it within the DataSet, and a version,
the newest of which wins, so that copies of a record converge wherever they are stored. IDs are SHA-256 hashes of
canonical JSON. Records are kept in files now, and the model maps directly onto a single DynamoDB table for later.

**Status:** working, in files on a computer and on an ESP32-S3's SD card, with a change feed for replication; its
format may still change before 1.0. See [docs/design.md](docs/design.md).

Its first user is [Marco Sparko](https://github.com/bruceskingle/marco-sparko), a desktop app for seeing a
household's energy, and the capture devices which poll its sources.

License
=======

Licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

Dependencies' licences
----------------------

A dependency of this crate is licensed MIT OR Apache-2.0, or another permissive licence where no such crate does the
job, and what it pulls in is permissive too: never copyleft. `cargo deny check`, see [deny.toml](deny.toml), checks every
crate in the build against that, listing by name each one which is not MIT OR Apache-2.0, e.g. `generic-array`, MIT,
which sha2 pulls in, and `unicode-normalization`, which is also under Unicode-3.0, the Unicode Consortium's MIT-like
licence for its tables. An app which ships this crate must ship the notices of the crates in it, which cargo-about can collect.

Contribution
------------

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
