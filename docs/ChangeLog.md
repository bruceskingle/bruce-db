# ChangeLog

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
