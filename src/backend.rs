//! The operations every store has, behind one trait, so that files now and DynamoDB later are two implementations of
//! it, see the design's operations.

use std::ops::Range;

use crate::id::Id;
use crate::item::{Item, Micros};

/// What a put did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Put {
    /// It was newer than any copy stored, so it is now the record.
    Stored,
    /// A copy as new or newer is stored, so it was ignored.
    Older,
}

/// Whether a visitor of a query wants more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// Where a peer got to in a node's changes: the sequence number of the last change it was given, see Change. A store
/// with one writer, a node's own files, gives each change it stores the next number, so one number is the whole
/// cursor; a store with many, DynamoDB, would need one for each writer, see the design's replication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cursor(pub u64);

impl Cursor {
    /// Before every change, for a peer which has none, as sequence numbers start at 1.
    pub const START: Cursor = Cursor(0);
}

/// A change, for replication: the record, its sequence number in this node's store, when it was stored, and the peer
/// it was pulled from, if it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub item: Item,
    pub sequence: u64,
    pub stored: Micros,
    pub from: Option<String>,
}

pub trait Backend: Send + Sync {
    /// Stores item unless a copy as new is stored, saying which happened. from is the peer it was pulled from, None
    /// if it is put here.
    fn put(&self, item: &Item, from: Option<&str>) -> anyhow::Result<Put>;

    /// Whether each put is synced to the disk before it returns, as it is unless this says otherwise, e.g. for a bulk
    /// load, which calls sync when it ends: a crash before then may lose what was put.
    fn set_sync(&self, sync_each_put: bool);

    /// Syncs what has been put to the disk, see set_sync.
    fn sync(&self) -> anyhow::Result<()>;

    /// The record, None if there is none or it has been deleted.
    fn get(&self, hk: &str, sk: &str) -> anyhow::Result<Option<Item>>;

    /// The newest version of each record of the DataSet hk with a sort key in range, in order, one at a time, leaving
    /// out tombstones, until visit says Stop.
    fn query(
        &self,
        hk: &str,
        range: Range<&str>,
        visit: &mut dyn FnMut(Item) -> anyhow::Result<Flow>,
    ) -> anyhow::Result<()>;

    /// Every change stored after since, in the order stored, up to limit, leaving out those from or originated by
    /// for_peer, so that a record is not sent back where it came from, returning the cursor to ask from next time.
    fn changes(
        &self,
        since: Cursor,
        limit: usize,
        for_peer: Option<&str>,
        visit: &mut dyn FnMut(Change) -> anyhow::Result<()>,
    ) -> anyhow::Result<Cursor>;

    /// The hash keys of the DataSets which start with prefix, e.g. every device's `climate.period` of a site.
    fn data_sets(&self, prefix: &str) -> anyhow::Result<Vec<String>>;

    /// Folds the journals of the days before before into the DataSets' files, see the design's layout.
    /// It says how far it is with progress, e.g. "Writing 1200 of 4800 files", as it can take a while after a bulk load,
    /// and stops if progress is cancelled, between files, returning Cancelled, leaving the journal to compact again.
    fn compact(&self, before: time::Date, progress: &mut dyn bruce_util_progress::Progress) -> anyhow::Result<()>;

    /// Stores bytes, e.g. a bill's PDF, by their ID, the SHA-256 of them, returning it.
    fn put_blob(&self, bytes: &[u8]) -> anyhow::Result<Id>;

    /// The bytes stored as id, None if there are none.
    fn get_blob(&self, id: &Id) -> anyhow::Result<Option<Vec<u8>>>;
}
