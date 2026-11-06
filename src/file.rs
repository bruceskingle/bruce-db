//! The file backend, see the design's layout on disk: a site's directory holds a journal, a file a day of every put
//! in the order it arrived, the DataSets' files, into which compaction folds the finished journal days, one line for
//! each record, ordered by sort key, and the blobs.
//!
//! ```text
//! <site hex>/
//!   journal/2026-11-04.jsonl     every put, as it arrived
//!   state.json                   the last sequence number given, and the newest compacted, see State
//!   blobs/ab/<hex>               a blob, by the hex of its ID
//!   <kind>/<parts>/records.jsonl a DataSet's records whose sort keys are not dates
//!   <kind>/<parts>/2026/11/04.jsonl  a day of a series
//! ```
//!
//! What is in the journal is also kept in memory, the newest version of each record, read again from where it got to
//! when the files have grown, so that a query does not read every journal day. That suits a computer; a device without
//! PSRAM would need to read the journal instead, which is not done yet.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, bail};

use crate::backend::{Backend, Change, Cursor, Flow, Put};
use crate::id::Id;
use crate::item::{Item, Line, Micros};
use crate::key::{self, RECORDS};

const JOURNAL: &str = "journal";
const BLOBS: &str = "blobs";
const STATE: &str = "state.json";

/// What the site's directory keeps of its own, saved by compaction.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct State {
    /// The last sequence number given, so that numbers keep going up once the journal they were in is compacted.
    last_sequence: Option<u64>,
    /// The latest sequence number of anything compacted, so that changes knows when it must read the DataSets' files
    /// as well as the journal.
    compacted_through: Option<u64>,
}

/// The journal in memory, see the module documentation.
#[derive(Default)]
struct Journal {
    /// How far each journal day's file has been read, by its name.
    read: BTreeMap<String, u64>,
    /// The newest version of each record in the journal, by hash key then sort key.
    records: HashMap<String, BTreeMap<String, Line>>,
    /// The latest sequence number in it.
    last_sequence: Option<u64>,
}

impl Journal {
    fn add(&mut self, line: Line) -> anyhow::Result<()> {
        self.last_sequence = self.last_sequence.max(Some(line.n));
        let records = self.records.entry(line.hk.clone()).or_default();
        let newer = match records.get(&line.sk) {
            Some(existing) => line.version()? > existing.version()?,
            None => true,
        };
        if newer {
            records.insert(line.sk.clone(), line);
        }
        Ok(())
    }
}

struct Inner {
    state: State,
    journal: Journal,
}

/// A site's store in files, see the module documentation.
pub struct FileBackend {
    dir: PathBuf,
    inner: Mutex<Inner>,
    /// Whether each put is synced to the disk before it returns, see Backend::set_sync.
    sync_each_put: std::sync::atomic::AtomicBool,
    /// The compacted files read most recently, by their records' sort keys, see compacted.
    compacted_files: Mutex<std::collections::VecDeque<CompactedFile>>,
}

/// A compacted file as it was read, see FileBackend::compacted, with its size and when it was modified, which say
/// whether it has changed since.
struct CompactedFile {
    path: PathBuf,
    length: u64,
    modified: std::time::SystemTime,
    records: std::sync::Arc<HashMap<String, Line>>,
}

/// How many compacted files are kept read, enough for puts which go through a few DataSets' days in turn.
const COMPACTED_FILES: usize = 8;

impl FileBackend {
    /// Copies the site from, in root, a directory of sites, as the new site to, e.g. for a copy of a profile, see
    /// Store::copy_site: each line of its journal and files with its hash key's site changed to to, and the rest of its
    /// files, its blobs and state, as they are. It fails if to's directory is there already, rather than mixing them.
    pub fn copy_site(root: &Path, from: &Id, to: &Id) -> anyhow::Result<()> {
        let (source, target) = (root.join(from.hex()), root.join(to.hex()));
        if target.exists() {
            bail!("The site {to} is in the store already");
        }
        let (from_prefix, to_prefix) = (format!("{from}/"), format!("{to}/"));
        let mut dirs = vec![PathBuf::new()];
        while let Some(relative) = dirs.pop() {
            fs::create_dir_all(target.join(&relative))?;
            for entry in fs::read_dir(source.join(&relative))?.flatten() {
                let name = relative.join(entry.file_name());
                if entry.path().is_dir() {
                    dirs.push(name);
                } else if name.extension().is_some_and(|extension| extension == "jsonl") {
                    let mut text = String::new();
                    read_lines(&entry.path(), &mut |mut line| {
                        if let Some(rest) = line.hk.strip_prefix(&from_prefix) {
                            line.hk = format!("{to_prefix}{rest}");
                        }
                        text.push_str(&serde_json::to_string(&line)?);
                        text.push('\n');
                        Ok(Flow::Continue)
                    })?;
                    write_atomically(&target.join(&name), text.as_bytes())?;
                } else if entry.file_name() != ".lock" {
                    fs::copy(entry.path(), target.join(&name))?;
                }
            }
        }
        Ok(())
    }

    /// The store of the site in root, a directory of sites, made if it is not there.
    pub fn open(root: &Path, site: &Id) -> anyhow::Result<FileBackend> {
        let dir = root.join(site.hex());
        fs::create_dir_all(dir.join(JOURNAL))
            .with_context(|| format!("Unable to make the store's directory {}", dir.display()))?;
        let state = match fs::read_to_string(dir.join(STATE)) {
            Ok(text) => serde_json::from_str(&text).with_context(|| format!("The store's {STATE} can not be read"))?,
            Err(_) => State::default(),
        };
        let backend = FileBackend {
            dir,
            inner: Mutex::new(Inner {
                state,
                journal: Journal::default(),
            }),
            sync_each_put: std::sync::atomic::AtomicBool::new(true),
            compacted_files: Mutex::new(std::collections::VecDeque::new()),
        };
        backend.refresh(&mut backend.inner.lock().unwrap())?;
        Ok(backend)
    }

    /// The site's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn journal_dir(&self) -> PathBuf {
        self.dir.join(JOURNAL)
    }

    /// Reads what has been added to the journal since it was last read, by this process or another, and reads it all
    /// again if a day has gone, compacted by another process.
    fn refresh(&self, inner: &mut Inner) -> anyhow::Result<()> {
        let mut days: Vec<String> = fs::read_dir(self.journal_dir())?
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter(|name| name.ends_with(".jsonl"))
            .collect();
        days.sort();
        if inner.journal.read.keys().any(|day| !days.contains(day)) {
            inner.journal = Journal::default();
        }
        for day in days {
            let path = self.journal_dir().join(&day);
            let length = fs::metadata(&path)?.len();
            let from = inner.journal.read.get(&day).copied().unwrap_or(0);
            if length <= from {
                continue;
            }
            let mut file = File::open(&path)?;
            file.seek(SeekFrom::Start(from))?;
            let mut reader = BufReader::new(file);
            let mut position = from;
            let mut text = String::new();
            loop {
                text.clear();
                let read = reader.read_line(&mut text)?;
                // A line without its newline is still being written, or was cut short by a crash, so it is left.
                if read == 0 || !text.ends_with('\n') {
                    break;
                }
                position += read as u64;
                match Line::parse(text.trim_end()) {
                    Ok(line) => inner.journal.add(line)?,
                    Err(error) => eprintln!("Skipping a line of {}: {error:#}", path.display()),
                }
            }
            inner.journal.read.insert(day, position);
        }
        Ok(())
    }

    /// The compacted line of the record, if there is one, from its file, which is kept read, see compacted_files, as a
    /// put looks for the record it replaces, and puts are often of one day's records after another.
    fn compacted(&self, hk: &str, sk: &str) -> anyhow::Result<Option<Line>> {
        let path = key::data_set_dir(&self.dir, hk)?.join(key::file_of(sk));
        let Ok(metadata) = fs::metadata(&path) else {
            return Ok(None);
        };
        let (length, modified) = (metadata.len(), metadata.modified()?);
        let mut files = self.compacted_files.lock().unwrap();
        let cached = files
            .iter()
            .position(|file| file.path == path)
            .and_then(|index| files.remove(index))
            .filter(|file| file.length == length && file.modified == modified);
        let file = match cached {
            Some(file) => file,
            None => {
                let mut records = HashMap::new();
                read_lines(&path, &mut |line| {
                    records.insert(line.sk.clone(), line);
                    Ok(Flow::Continue)
                })?;
                CompactedFile {
                    path,
                    length,
                    modified,
                    records: std::sync::Arc::new(records),
                }
            }
        };
        let found = file.records.get(sk).filter(|line| line.hk == hk).cloned();
        files.push_front(file);
        files.truncate(COMPACTED_FILES);
        Ok(found)
    }

    /// The newest version of the record, journal or compacted, tombstones included.
    fn newest(&self, inner: &Inner, hk: &str, sk: &str) -> anyhow::Result<Option<Line>> {
        let journal = inner
            .journal
            .records
            .get(hk)
            .and_then(|records| records.get(sk))
            .cloned();
        let compacted = self.compacted(hk, sk)?;
        Ok(match (journal, compacted) {
            (Some(journal), Some(compacted)) => Some(if journal.version()? >= compacted.version()? {
                journal
            } else {
                compacted
            }),
            (journal, compacted) => journal.or(compacted),
        })
    }

    /// The next sequence number to store with, one after the last given, see State.
    fn next_sequence(inner: &Inner) -> u64 {
        inner.journal.last_sequence.max(inner.state.last_sequence).unwrap_or(0) + 1
    }

    /// Every compacted line of the site, for changes from before the journal.
    fn all_compacted(&self, visit: &mut dyn FnMut(Line) -> anyhow::Result<Flow>) -> anyhow::Result<()> {
        let mut dirs = vec![self.dir.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in fs::read_dir(&dir)?.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if path.is_dir() {
                    if dir == self.dir && (name == JOURNAL || name == BLOBS) {
                        continue;
                    }
                    dirs.push(path);
                } else if name.ends_with(".jsonl") {
                    read_lines(&path, visit)?;
                }
            }
        }
        Ok(())
    }

    fn save_state(&self, state: &State) -> anyhow::Result<()> {
        write_atomically(&self.dir.join(STATE), serde_json::to_string(state)?.as_bytes())
    }

    fn blob_path(&self, id: &Id) -> PathBuf {
        let hex = id.hex();
        self.dir.join(BLOBS).join(&hex[0..2]).join(hex)
    }
}

/// Reads each line of a file, if it is there, until visit says Stop.
fn read_lines(path: &Path, visit: &mut dyn FnMut(Line) -> anyhow::Result<Flow>) -> anyhow::Result<()> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("Unable to read {}", path.display())),
    };
    for text in BufReader::new(file).lines() {
        let text = text?;
        if text.trim().is_empty() {
            continue;
        }
        if visit(Line::parse(&text).with_context(|| format!("In {}", path.display()))?)? == Flow::Stop {
            break;
        }
    }
    Ok(())
}

/// Writes a file to a temporary name and renames it over the old, so that a crash leaves one or the other whole.
fn write_atomically(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    {
        let mut file = File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path).with_context(|| format!("Unable to write {}", path.display()))?;
    Ok(())
}

/// Whether a day's file, whose records' sort keys all start with day, e.g. 2026-11-04, may hold any in range.
fn day_in_range(day: &str, range: &Range<&str>) -> bool {
    // Every sort key in the file is from day up to, but not including, day followed by the highest character.
    day < range.end && range.start < format!("{day}\u{10FFFF}").as_str()
}

/// Holds the journal's lock, so that two processes do not append at once, released when dropped. ESP-IDF's file
/// system has no locks, and a device has one writer.
struct JournalLock {
    #[cfg(not(target_os = "espidf"))]
    _file: File,
}

impl JournalLock {
    fn take(journal_dir: &Path) -> anyhow::Result<JournalLock> {
        #[cfg(not(target_os = "espidf"))]
        {
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(journal_dir.join(".lock"))?;
            file.lock()?;
            Ok(JournalLock { _file: file })
        }
        #[cfg(target_os = "espidf")]
        {
            let _ = journal_dir;
            Ok(JournalLock {})
        }
    }
}

impl Backend for FileBackend {
    fn put(&self, item: &Item, from: Option<&str>) -> anyhow::Result<Put> {
        if key::site_of(&item.hk)?.hex() != self.dir.file_name().and_then(|name| name.to_str()).unwrap_or_default() {
            bail!("{} is not a hash key of this store's site", item.hk);
        }
        key::data_set_dir(&self.dir, &item.hk)?;
        let _lock = JournalLock::take(&self.journal_dir())?;
        let mut inner = self.inner.lock().unwrap();
        self.refresh(&mut inner)?;
        if let Some(existing) = self.newest(&inner, &item.hk, &item.sk)? {
            if existing.version()? >= item.version {
                return Ok(Put::Older);
            }
        }
        let stored = Micros::now();
        let line = Line::new(item, Self::next_sequence(&inner), stored, from);
        let path = self.journal_dir().join(format!("{}.jsonl", stored.date()?));
        let mut text = serde_json::to_string(&line)?;
        text.push('\n');
        let day = format!("{}.jsonl", stored.date()?);
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        let before = file.metadata()?.len();
        file.write_all(text.as_bytes())?;
        if self.sync_each_put.load(std::sync::atomic::Ordering::Relaxed) {
            file.sync_data()?;
        }
        drop(file);
        if inner.journal.read.get(&day).copied().unwrap_or(0) == before {
            // Nothing else was added since it was read, so the line is added to what is in memory, and read past.
            inner.journal.add(line)?;
            inner.journal.read.insert(day, before + text.len() as u64);
        } else {
            self.refresh(&mut inner)?;
        }
        Ok(Put::Stored)
    }

    fn set_sync(&self, sync_each_put: bool) {
        self.sync_each_put
            .store(sync_each_put, std::sync::atomic::Ordering::Relaxed);
    }

    fn sync(&self) -> anyhow::Result<()> {
        for entry in fs::read_dir(self.journal_dir())?.flatten() {
            if entry.file_name().to_string_lossy().ends_with(".jsonl") {
                File::open(entry.path())?.sync_all()?;
            }
        }
        Ok(())
    }

    fn get(&self, hk: &str, sk: &str) -> anyhow::Result<Option<Item>> {
        let mut inner = self.inner.lock().unwrap();
        self.refresh(&mut inner)?;
        match self.newest(&inner, hk, sk)? {
            Some(line) if line.a.is_some() => Ok(Some(line.item()?)),
            _ => Ok(None),
        }
    }

    fn query(
        &self,
        hk: &str,
        range: Range<&str>,
        visit: &mut dyn FnMut(Item) -> anyhow::Result<Flow>,
    ) -> anyhow::Result<()> {
        let mut found: BTreeMap<String, Line> = BTreeMap::new();
        let keep = |line: Line, found: &mut BTreeMap<String, Line>| -> anyhow::Result<()> {
            if line.hk != hk || line.sk.as_str() < range.start || line.sk.as_str() >= range.end {
                return Ok(());
            }
            let newer = match found.get(&line.sk) {
                Some(existing) => line.version()? > existing.version()?,
                None => true,
            };
            if newer {
                found.insert(line.sk.clone(), line);
            }
            Ok(())
        };
        let dir = key::data_set_dir(&self.dir, hk)?;
        read_lines(&dir.join(RECORDS), &mut |line| {
            keep(line, &mut found)?;
            Ok(Flow::Continue)
        })?;
        for year in sorted_entries(&dir)? {
            for month in sorted_entries(&dir.join(&year))? {
                for day_file in sorted_entries(&dir.join(&year).join(&month))? {
                    let Some(day) = day_file.strip_suffix(".jsonl") else {
                        continue;
                    };
                    if year.len() == 4 && day_in_range(&format!("{year}-{month}-{day}"), &range) {
                        read_lines(&dir.join(&year).join(&month).join(&day_file), &mut |line| {
                            keep(line, &mut found)?;
                            Ok(Flow::Continue)
                        })?;
                    }
                }
            }
        }
        {
            let mut inner = self.inner.lock().unwrap();
            self.refresh(&mut inner)?;
            if let Some(records) = inner.journal.records.get(hk) {
                for line in records
                    .range(range.start.to_string()..range.end.to_string())
                    .map(|(_, record)| record)
                {
                    keep(line.clone(), &mut found)?;
                }
            }
        }
        for line in found.into_values() {
            if line.a.is_some() && visit(line.item()?)? == Flow::Stop {
                break;
            }
        }
        Ok(())
    }

    fn changes(
        &self,
        since: Cursor,
        limit: usize,
        for_peer: Option<&str>,
        visit: &mut dyn FnMut(Change) -> anyhow::Result<()>,
    ) -> anyhow::Result<Cursor> {
        let (journal, compacted_through) = {
            let mut inner = self.inner.lock().unwrap();
            self.refresh(&mut inner)?;
            let journal: Vec<Line> = inner
                .journal
                .records
                .values()
                .flat_map(|records| records.values())
                .filter(|line| line.n > since.0)
                .cloned()
                .collect();
            (journal, inner.state.compacted_through)
        };
        // The newest of each record, from the journal and, if since is before the end of what was compacted, from the
        // DataSets' files, which keep each line's sequence number.
        let mut newest: HashMap<(String, String), Line> = HashMap::new();
        let mut keep = |line: Line| -> anyhow::Result<()> {
            let key = (line.hk.clone(), line.sk.clone());
            let newer = match newest.get(&key) {
                Some(existing) => line.version()? > existing.version()?,
                None => true,
            };
            if newer {
                newest.insert(key, line);
            }
            Ok(())
        };
        if compacted_through.is_some_and(|through| through > since.0) {
            let mut lines = Vec::new();
            self.all_compacted(&mut |line| {
                if line.n > since.0 {
                    lines.push(line);
                }
                Ok(Flow::Continue)
            })?;
            for line in lines {
                keep(line)?;
            }
        }
        for line in journal {
            keep(line)?;
        }
        let mut changes: Vec<Line> = newest.into_values().collect();
        changes.sort_by_key(|line| line.n);
        let mut cursor = since;
        let mut sent = 0;
        for line in changes {
            if sent == limit {
                break;
            }
            cursor = Cursor(line.n);
            let from_peer = for_peer.is_some_and(|peer| line.from.as_deref() == Some(peer) || line.o == peer);
            if from_peer {
                continue;
            }
            visit(Change {
                item: line.item()?,
                sequence: line.n,
                stored: line.stored()?,
                from: line.from.clone(),
            })?;
            sent += 1;
        }
        Ok(cursor)
    }

    fn data_sets(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let mut found = std::collections::BTreeSet::new();
        {
            let mut inner = self.inner.lock().unwrap();
            self.refresh(&mut inner)?;
            found.extend(
                inner
                    .journal
                    .records
                    .keys()
                    .filter(|hk| hk.starts_with(prefix))
                    .cloned(),
            );
        }
        let site = self
            .dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(Id::from_hex)
            .transpose()?;
        let Some(site) = site else {
            return Ok(found.into_iter().collect());
        };
        let mut dirs: Vec<(PathBuf, String)> = Vec::new();
        for entry in fs::read_dir(&self.dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.path().is_dir() && name != JOURNAL && name != BLOBS && key::is_name(&name) {
                dirs.push((entry.path(), format!("{site}/{name}")));
            }
        }
        while let Some((dir, hk)) = dirs.pop() {
            let mut is_data_set = dir.join(RECORDS).exists();
            for entry in fs::read_dir(&dir)?.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !entry.path().is_dir() {
                    continue;
                }
                if name.len() == 4 && name.bytes().all(|byte| byte.is_ascii_digit()) {
                    is_data_set = true;
                    continue;
                }
                let part = if name.len() == 64 {
                    Id::from_hex(&name)?.to_string()
                } else {
                    name
                };
                dirs.push((entry.path(), format!("{hk}/{part}")));
            }
            if is_data_set && hk.starts_with(prefix) {
                found.insert(hk);
            }
        }
        Ok(found.into_iter().collect())
    }

    fn compact(&self, before: time::Date, progress: &mut dyn bruce_util_progress::Progress) -> anyhow::Result<()> {
        let _lock = JournalLock::take(&self.journal_dir())?;
        let mut inner = self.inner.lock().unwrap();
        let mut days: Vec<String> = fs::read_dir(self.journal_dir())?
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter(|name| name.ends_with(".jsonl") && name.as_str() < format!("{before}.jsonl").as_str())
            .collect();
        days.sort();
        if days.is_empty() {
            return Ok(());
        }
        // The newest of each record in those days, by the file it goes in.
        let mut files: BTreeMap<PathBuf, BTreeMap<String, Line>> = BTreeMap::new();
        let mut latest = inner.state.compacted_through;
        for (index, day) in days.iter().enumerate() {
            let mut lines = 0usize;
            read_lines(&self.journal_dir().join(day), &mut |line| {
                lines += 1;
                if lines % 100_000 == 0 {
                    progress.report(&format!(
                        "Reading the journal, day {} of {}: {lines} lines",
                        index + 1,
                        days.len()
                    ));
                }
                latest = latest.max(Some(line.n));
                let path = key::data_set_dir(&self.dir, &line.hk)?.join(key::file_of(&line.sk));
                let records = files.entry(path).or_default();
                let newer = match records.get(&line.sk) {
                    Some(existing) => line.version()? > existing.version()?,
                    None => true,
                };
                if newer {
                    records.insert(line.sk.clone(), line);
                }
                Ok(Flow::Continue)
            })?;
        }
        let count = files.len();
        for (index, (path, records)) in files.into_iter().enumerate() {
            // Stopping between files leaves each file whole, and the journal, which is only deleted at the end, to be
            // compacted again, which gives the same files.
            progress.check()?;
            bruce_util_progress::report_count(progress, index, count, "files written");
            // A file holds one DataSet's records, so by sort key alone.
            let mut merged: BTreeMap<String, Line> = BTreeMap::new();
            read_lines(&path, &mut |line| {
                merged.insert(line.sk.clone(), line);
                Ok(Flow::Continue)
            })?;
            for (sk, line) in records {
                let newer = match merged.get(&sk) {
                    Some(existing) => line.version()? > existing.version()?,
                    None => true,
                };
                if newer {
                    merged.insert(sk, line);
                }
            }
            let mut text = String::new();
            for line in merged.values() {
                text.push_str(&serde_json::to_string(line)?);
                text.push('\n');
            }
            write_atomically(&path, text.as_bytes())?;
        }
        inner.state = State {
            last_sequence: inner.journal.last_sequence.max(inner.state.last_sequence),
            compacted_through: latest,
        };
        self.save_state(&inner.state)?;
        for day in &days {
            fs::remove_file(self.journal_dir().join(day))?;
        }
        inner.journal = Journal::default();
        // The files written may not look changed, as FAT keeps their times to 2 seconds.
        self.compacted_files.lock().unwrap().clear();
        self.refresh(&mut inner)?;
        Ok(())
    }

    fn put_blob(&self, bytes: &[u8]) -> anyhow::Result<Id> {
        let id = Id::of_bytes(bytes);
        let path = self.blob_path(&id);
        if !path.exists() {
            write_atomically(&path, bytes)?;
        }
        Ok(id)
    }

    fn get_blob(&self, id: &Id) -> anyhow::Result<Option<Vec<u8>>> {
        match fs::read(self.blob_path(id)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

/// The names of the entries of a directory, sorted, none if it is not there.
fn sorted_entries(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut names: Vec<String> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    Ok(names)
}
