//! Bruce's Database: a record store with a hash key, a sort key and a version on every record, see docs/design.md.
//!
//! A store is opened for a domain, by its location, see Store::open, and gives the operations of the Backend, whichever
//! one it is, so that the code using it never names one.

mod backend;
mod file;
mod id;
mod item;
mod key;
pub mod security;

use std::ops::Range;
use std::path::PathBuf;

use anyhow::bail;

pub use backend::{Backend, Change, CopyPage, CopyToken, Cursor, Feed, Flow, Put};
/// Progress and cancellation of long work, e.g. compaction, see bruce-util-progress.
pub use bruce_util_progress as progress;
pub use file::FileBackend;
pub use id::{Id, is_id};
pub use item::{Item, Micros, Version};
pub use key::{Part, domain_of, hash_key, is_name, sort_key_date};

/// A domain's store, see the crate documentation.
pub struct Store {
    domain: Id,
    backend: Box<dyn Backend>,
}

impl Store {
    /// The store of domain at location, e.g. `file:///Users/bruce/Library/…/store`, a directory of domains, the only
    /// kind of location there is so far.
    pub fn open(location: &str, domain: &Id) -> anyhow::Result<Store> {
        match location.strip_prefix("file://") {
            Some(path) => Ok(Store {
                domain: domain.clone(),
                backend: Box::new(FileBackend::open(&PathBuf::from(path), domain)?),
            }),
            None => bail!("{location} is not a store's location: only file:// is known"),
        }
    }

    /// Copies the domain from, at location, as the new domain to, every record the same but for its hash key's domain,
    /// see FileBackend::copy_domain, e.g. for a copy of a profile.
    pub fn copy_domain(location: &str, from: &Id, to: &Id) -> anyhow::Result<()> {
        match location.strip_prefix("file://") {
            Some(path) => FileBackend::copy_domain(&PathBuf::from(path), from, to),
            None => bail!("{location} is not a store's location: only file:// is known"),
        }
    }

    /// A store of domain with backend, e.g. for tests.
    pub fn with_backend(domain: &Id, backend: Box<dyn Backend>) -> Store {
        Store {
            domain: domain.clone(),
            backend,
        }
    }

    pub fn domain(&self) -> &Id {
        &self.domain
    }

    /// See Backend::store_id.
    pub fn store_id(&self) -> String {
        self.backend.store_id()
    }

    /// The hash key of a DataSet of the kind in this store's domain, see key::hash_key.
    pub fn hash_key(&self, kind: &str, parts: &[Part]) -> anyhow::Result<String> {
        hash_key(&self.domain, kind, parts)
    }

    pub fn put(&self, item: &Item) -> anyhow::Result<Put> {
        self.backend.put(item, None)
    }

    /// Puts a record pulled from peer, see Backend::put.
    pub fn put_from(&self, item: &Item, peer: &str) -> anyhow::Result<Put> {
        self.backend.put(item, Some(peer))
    }

    /// See Backend::set_sync.
    pub fn set_sync(&self, sync_each_put: bool) {
        self.backend.set_sync(sync_each_put)
    }

    /// See Backend::sync.
    pub fn sync(&self) -> anyhow::Result<()> {
        self.backend.sync()
    }

    pub fn get(&self, hk: &str, sk: &str) -> anyhow::Result<Option<Item>> {
        self.backend.get(hk, sk)
    }

    pub fn query(
        &self,
        hk: &str,
        range: Range<&str>,
        visit: &mut dyn FnMut(Item) -> anyhow::Result<Flow>,
    ) -> anyhow::Result<()> {
        self.backend.query(hk, range, visit)
    }

    /// Every record of the DataSet hk with a sort key in range, in order. For a range known to be small; a large one is
    /// better visited, see query.
    pub fn query_all(&self, hk: &str, range: Range<&str>) -> anyhow::Result<Vec<Item>> {
        let mut items = Vec::new();
        self.backend.query(hk, range, &mut |item| {
            items.push(item);
            Ok(Flow::Continue)
        })?;
        Ok(items)
    }

    /// Every record of the DataSet hk.
    pub fn all(&self, hk: &str) -> anyhow::Result<Vec<Item>> {
        self.query_all(hk, ALL)
    }

    /// A page of the change feed, see Backend::changes.
    pub fn changes(
        &self,
        since: Cursor,
        limit: usize,
        for_peer: Option<&str>,
        visit: &mut dyn FnMut(Change) -> anyhow::Result<()>,
    ) -> anyhow::Result<Feed> {
        self.backend.changes(since, limit, for_peer, visit)
    }

    /// A page of a full copy, for a peer too far behind for the change feed, see Backend::copy.
    pub fn copy(
        &self,
        token: Option<&CopyToken>,
        limit: usize,
        for_peer: Option<&str>,
        visit: &mut dyn FnMut(Change) -> anyhow::Result<()>,
    ) -> anyhow::Result<CopyPage> {
        self.backend.copy(token, limit, for_peer, visit)
    }

    pub fn data_sets(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        self.backend.data_sets(prefix)
    }

    pub fn compact(&self, before: time::Date) -> anyhow::Result<()> {
        self.backend.compact(before, &mut progress::Silent)
    }

    /// Compacts, see Backend::compact, saying how far it is with progress, and stopping if it is cancelled.
    pub fn compact_with_progress(
        &self,
        before: time::Date,
        progress: &mut dyn progress::Progress,
    ) -> anyhow::Result<()> {
        self.backend.compact(before, progress)
    }

    /// Compacts the hours of the journal which ended by before, e.g. now, see Backend::compact_through.
    pub fn compact_through(&self, before: Micros) -> anyhow::Result<()> {
        self.backend.compact_through(before, &mut progress::Silent)
    }

    /// Compacts the hours which ended by before, saying how far it is with progress, and stopping if it is cancelled.
    pub fn compact_through_with_progress(
        &self,
        before: Micros,
        progress: &mut dyn progress::Progress,
    ) -> anyhow::Result<()> {
        self.backend.compact_through(before, progress)
    }

    /// How long compacted journal files are kept, see Backend::set_retention.
    pub fn set_retention(&self, retention: std::time::Duration) {
        self.backend.set_retention(retention)
    }

    pub fn put_blob(&self, bytes: &[u8]) -> anyhow::Result<Id> {
        self.backend.put_blob(bytes)
    }

    pub fn get_blob(&self, id: &Id) -> anyhow::Result<Option<Vec<u8>>> {
        self.backend.get_blob(id)
    }
}

/// Every sort key, from the lowest, `#` and digits, to the highest character.
pub const ALL: Range<&str> = "\0".."\u{10FFFF}";

#[cfg(test)]
mod tests;
