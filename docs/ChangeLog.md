# ChangeLog

# Release 0.4.0

## New

- **The full copy,** `Store::copy`, for a peer further behind than the journal kept, e.g. a new device: a page at a
  time of the DataSets' files, every record once, with a `CopyToken` the peer passes back for the next page, then
  `CopyPage::Done` with the cursor to catch up from with the change feed, which brings what changed while it copied.
  A copy which outlasts the journal kept, or is of a store since replaced, answers `CopyPage::Expired`, and is started
  again.
- **Security Contexts, the first parts,** `security`, see the design's "Security Contexts and Principals": a domain,
  `Founding`, the root SC, whose ID is made from its founder's first key, which is the store's domain; principals, users
  and systems, `NewPrincipal`, `PrincipalSecret` and `PrincipalRecord`, each a DataSet of its domain, admitted by an
  Admin of it, with its keys, `KeyRecord`, each an Ed25519 signing key and an X25519 key exchange key, committing to
  the next, and valid until its `until`, and what is said of it, `DirectoryRecord`, e.g. its name; SCs, `ScRecord`,
  joined into a graph by membership, `MemberRecord`, Admin, Write or Read, each change a record of its own, signed by
  an Admin; delegations, `DelegationRecord`, by which an actor acts for its subject while the subject is away, up to
  Write; `Security::load`, which works out each principal's role, the best of its paths through the graph, from the
  store's records, judged in the order they were signed, so that a removal is not undone and two memberships never
  make each other valid; `sign_request`, `sign_request_for` and `Security::check_request`, for requests signed by
  their subject and actor, and `Security::role_for`, what such a request may do; and `Security::sees_security_records`
  and `is_security_data_set`, for a node to give the security records only to those entitled to them. A security
  record's version is when it was signed. The DataSets of principals and of what is said of them are split on disk by
  their IDs' first two hex characters.
- **A gap in the journal kept is found,** e.g. an hour lost from a failing SD card: a peer whose catch-up would cross
  it is told it is too far behind, and copies instead, rather than missing the changes in it without knowing.

## Improved

- The change feed, `Store::changes`, answers `Feed::TooFarBehind` for a cursor older than the journal kept, rather
  than reading every DataSet's file to find the changes after it, which took minutes on a device for its first page.
  It returns `Feed::Next` with the cursor otherwise. This changes its signature: callers make a full copy when they are
  too far behind.

## Fixed

- **A record whose line is damaged on the disk is missing,** not an error, even when the store had read the line
  before it was damaged, and compaction goes on without it, rather than failing until the hour is deleted. The same
  version of the record put again, e.g. sent by a peer, is stored in its place, so that a peer's copy repairs it.

# Release 0.3.0

## New

- The journal is written a file an hour, e.g. `journal/2026-11-04T07.jsonl`, rather than a day, and
  `compact_through(time)` compacts the hours which ended by then, so that a device can compact a little at a time.
  A store written by an earlier version is read as it is, its daily files before their days' hours, and marked as
  format 2, which earlier versions refuse.
- Compacted hours of the journal are kept, in `journal/kept/`, for three days, or as long as `set_retention` says, so
  that a peer up to that far behind catches up from them quickly, rather than from the DataSets' files.
- Each line written ends with a CRC32 of its text, `"c"`, so that a line damaged on the disk, e.g. by a failing SD
  card, is found even when it is still JSON. A damaged or unreadable line is skipped, and logged, rather than stopping
  the store from opening; lines written before have no checksum and are taken as they are.

## Improved

- Catching up from a sequence number behind compaction, beyond the kept hours, remembers which compacted files hold
  which sequence numbers, so that only the first page is slow, not every page.
- Compaction appends to a DataSet's file when the new records sort after its last, rather than rewriting it.
- A query for a range of sort keys seeks to the range's start in a DataSet's file, rather than reading it from the
  beginning.
- Lines are checked with a table-driven CRC32, so checking every line read costs little.

# Release 0.2.1

## Fixed

- Syncing the store, after puts which were not synced one by one, failed on Windows with "Access is denied", as each
  journal file was opened read-only to be synced.

# Release 0.2.0

## New

- How long opening a store and compacting it took is logged at info, with the lines, records and files involved, and
  so is any reading of the journal which takes over a second, so that an application can see where its time goes.

## Improved

- Compaction streams each file's records from the journal's index, a line at a time, rather than listing every record
  of the days it folds first, so that a device can compact several days of journal without running out of memory.
- Reading the journal's lines on a device keeps its buffer from one line to the next, rather than throwing it away,
  which made compaction several times slower there.
- The design, docs/design.md, describes the store for any application, with a new section on using the crate: opening,
  scheduling compaction, bulk loads, logging, and what a device needs.

## Fixed

- Compaction failed on ESP-IDF's FAT, where a file cannot be renamed onto one which exists: the old file is now moved
  aside first, and put back when the file is next read if a crash came between.
- Compaction failed with too many open files on a device whose FAT allows only a few at once.

# Release 0.1.0

## New

- The record model, hashed IDs, the file backend with its journal and compaction, the change feed for replication,
  the store's ID, and blobs, for computers and for ESP32-S3 devices with PSRAM.
