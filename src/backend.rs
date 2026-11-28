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

/// What a page of the change feed says, see Backend::changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feed {
    /// The changes after since were given, up to the limit, and the peer asks from this cursor next time, the same as
    /// since if there were none.
    Next(Cursor),
    /// since is older than the journal this node keeps, so the changes after it can no longer be given in the order
    /// stored: the peer makes a full copy, see Backend::copy, and then asks from the cursor that gives.
    TooFarBehind,
}

/// Where a full copy has got to, see Backend::copy: a token the peer keeps between pages and passes back unread, e.g.
/// in a URL, as it is URL safe text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyToken(pub String);

/// What a page of a full copy says, see Backend::copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyPage {
    /// There is more: the peer asks for the next page with this token.
    More(CopyToken),
    /// The copy is complete, and the peer catches up with the change feed from this cursor, which brings everything
    /// changed while it was copied.
    Done(Cursor),
    /// The copy took longer than the journal this node keeps, so a change to a part already copied could be missed:
    /// the peer starts again, keeping what it has, as putting it again changes nothing.
    Expired,
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

impl Change {
    /// The change as a journal's line is, `{"n","t","from","o","hk","sk","v","a"}`, which is how a node's feed gives
    /// it to a peer, e.g. a capture device's `/api/store/changes`, so that the feed and the files have one format.
    pub fn to_json(&self) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::to_value(crate::item::Line::new(
            &self.item,
            self.sequence,
            self.stored,
            self.from.as_deref(),
        ))?)
    }

    /// A change given by a peer's feed, see to_json.
    pub fn from_json(json: serde_json::Value) -> anyhow::Result<Change> {
        let line: crate::item::Line = serde_json::from_value(json)?;
        Ok(Change {
            item: line.item()?,
            sequence: line.n,
            stored: line.stored()?,
            from: line.from.clone(),
        })
    }
}

pub trait Backend: Send + Sync {
    /// The store's ID, made when it was first made, which a peer keeps with its cursor, see changes, so that it can
    /// tell when a node's store is a new one, e.g. a device's wiped or changed card, whose sequence numbers start
    /// again.
    fn store_id(&self) -> String;

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
    /// for_peer, so that a record is not sent back where it came from, returning the cursor to ask from next time, or,
    /// if since is older than the journal kept, that the peer is too far behind, and makes a full copy, see copy.
    fn changes(
        &self,
        since: Cursor,
        limit: usize,
        for_peer: Option<&str>,
        visit: &mut dyn FnMut(Change) -> anyhow::Result<()>,
    ) -> anyhow::Result<Feed>;

    /// A page of a full copy of the store, for a peer too far behind for the change feed, see Feed::TooFarBehind: up to
    /// limit of its records, from where token says the last page got to, or from the start if None, leaving out those
    /// from or originated by for_peer, as changes does. Each record is given as a Change, with the sequence number it
    /// was stored with, and a peer puts them as it puts changes. See the design's "The journal by the hour".
    fn copy(
        &self,
        token: Option<&CopyToken>,
        limit: usize,
        for_peer: Option<&str>,
        visit: &mut dyn FnMut(Change) -> anyhow::Result<()>,
    ) -> anyhow::Result<CopyPage>;

    /// The hash keys of the DataSets which start with prefix, e.g. every device's `climate.period` of a domain.
    fn data_sets(&self, prefix: &str) -> anyhow::Result<Vec<String>>;

    /// Folds the journals of the days before before into the DataSets' files, see the design's layout. It says how far
    /// it is with progress, e.g. "Writing 1200 of 4800 files", as it can take a while after a bulk load, and stops if
    /// progress is cancelled, between files, returning Cancelled, leaving the journal to compact again.
    fn compact(&self, before: time::Date, progress: &mut dyn bruce_util_progress::Progress) -> anyhow::Result<()>;

    /// Folds the journal's files of the hours which ended by before, e.g. now, into the DataSets' files, as compact
    /// does the days, so that the journal stays an hour or two, which is what opening the store reads, see the design's
    /// journal by the hour. The hours folded are kept, see set_retention, for peers catching up.
    fn compact_through(
        &self,
        before: crate::item::Micros,
        progress: &mut dyn bruce_util_progress::Progress,
    ) -> anyhow::Result<()>;

    /// How long the journal's files are kept once they are compacted, three days unless this says otherwise, so that a
    /// peer which is behind catches up from them, in the order they were stored, rather than from the DataSets' files.
    fn set_retention(&self, retention: std::time::Duration);

    /// Stores bytes, e.g. a bill's PDF, by their ID, the SHA-256 of them, returning it.
    fn put_blob(&self, bytes: &[u8]) -> anyhow::Result<Id>;

    /// The bytes stored as id, None if there are none.
    fn get_blob(&self, id: &Id) -> anyhow::Result<Option<Vec<u8>>>;
}
