# ChangeLog

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
