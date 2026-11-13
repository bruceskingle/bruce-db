//! The file backend, see the design's layout on disk: a site's directory holds a journal, a file a day of every put
//! in the order it arrived, the DataSets' files, into which compaction folds the finished journal days, one line for
//! each record, ordered by sort key, and the blobs.
//!
//! ```text
//! <site hex>/
//!   journal/2026-11-04.jsonl     every put, as it arrived
//!   state.json                   the store's ID, the last sequence number given, and the newest compacted, see State
//!   blobs/ab/<hex>               a blob, by the hex of its ID
//!   <kind>/<parts>/records.jsonl a DataSet's records whose sort keys are not dates
//!   <kind>/<parts>/2026/11/04.jsonl  a day of a series
//! ```
//!
//! What is in the journal is indexed in memory, read again from where it got to when the files have grown, so that a
//! put, a get or a query does not read every journal day: for each record its newest line's sequence number, version
//! and where the line is, not its attributes, which are read from the file when they are wanted. A day of 10 second
//! Tesla snapshots is about 10 MB of lines but about 1 MB of index, which a capture device's PSRAM holds, see the
//! design's capture devices. Queries, the change feed and compaction read the files a line at a time, so that none of
//! them holds more than the index and the lines it is working on.

use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::ops::{Bound, Range};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use log::{info, warn};

use crate::backend::{Backend, Change, Cursor, Flow, Put};
use crate::id::Id;
use crate::item::{Head, Item, Line, Micros, Version};
use crate::key::{self, RECORDS};

const JOURNAL: &str = "journal";
const BLOBS: &str = "blobs";
const STATE: &str = "state.json";

/// How much of a file is read at a time, small enough for a device, as lines are read from many files at once.
const READ_BUFFER: usize = 4096;

/// How long a refresh may take before it is logged, as one that reads only the lines put since the last is quick, but
/// one that reads the whole journal again, after another process compacted it, is what a page waits for.
const SLOW_REFRESH: Duration = Duration::from_secs(1);

/// What a refresh of the journal's index read, for the log, see FileBackend::refresh.
#[derive(Debug, Default, Clone, Copy)]
struct Indexed {
    /// The lines indexed.
    lines: u64,
    /// The bytes of them.
    bytes: u64,
    /// Whether the index was made again from the start, as a day had been compacted by another process.
    again: bool,
}

/// What the site's directory keeps of its own.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct State {
    /// The store's ID, made when it is first opened, so that a peer can tell when a node's store is a new one, e.g. a
    /// wiped or changed card, whose sequence numbers start again, see Backend::store_id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    /// The last sequence number given, so that numbers keep going up once the journal they were in is compacted.
    last_sequence: Option<u64>,
    /// The latest sequence number of anything compacted, so that changes knows when it must read the DataSets' files
    /// as well as the journal.
    compacted_through: Option<u64>,
}

/// A record's newest line in the journal, without its attributes, see the module documentation: its sequence number,
/// its version, with its origin as an index into Journal::origins, as there are few, and where the line is, its day as
/// an index into Journal::days and its offset in that day's file.
#[derive(Debug, Clone, Copy)]
struct Entry {
    n: u64,
    time: Micros,
    origin: u32,
    day: u32,
    offset: u64,
}

/// The index of the journal, see the module documentation.
#[derive(Default)]
struct Journal {
    /// The journal's days which have been read, by file name, e.g. 2026-11-04.jsonl, with how far each has been read.
    days: Vec<(String, u64)>,
    /// The origins of the entries, each once.
    origins: Vec<String>,
    /// The newest entry of each record in the journal, by hash key then sort key.
    records: HashMap<Arc<str>, BTreeMap<Box<str>, Entry>>,
    /// The latest sequence number in it.
    last_sequence: Option<u64>,
}

impl Journal {
    /// The index of the day's file, e.g. 2026-11-04.jsonl, in days, added if it is not there.
    fn day(&mut self, name: &str) -> u32 {
        match self.days.iter().position(|(day, _)| day == name) {
            Some(index) => index as u32,
            None => {
                self.days.push((name.to_string(), 0));
                (self.days.len() - 1) as u32
            }
        }
    }

    /// How far the day's file has been read.
    fn read_to(&self, name: &str) -> u64 {
        self.days
            .iter()
            .find(|(day, _)| day == name)
            .map_or(0, |(_, position)| *position)
    }

    fn origin(&mut self, origin: &str) -> u32 {
        match self.origins.iter().position(|known| known == origin) {
            Some(index) => index as u32,
            None => {
                self.origins.push(origin.to_string());
                (self.origins.len() - 1) as u32
            }
        }
    }

    fn version(&self, entry: &Entry) -> Version {
        Version::new(entry.time, &self.origins[entry.origin as usize])
    }

    fn get(&self, hk: &str, sk: &str) -> Option<&Entry> {
        self.records.get(hk).and_then(|records| records.get(sk))
    }

    /// Indexes the line head, at offset in the day's file, if it is the newest of its record.
    fn add(&mut self, head: &Head, day: u32, offset: u64) -> anyhow::Result<()> {
        self.last_sequence = self.last_sequence.max(Some(head.n));
        let entry = Entry {
            n: head.n,
            time: Micros::parse(&head.v)?,
            origin: self.origin(&head.o),
            day,
            offset,
        };
        let newer = match self.get(&head.hk, &head.sk) {
            Some(existing) => self.version(&entry) > self.version(existing),
            None => true,
        };
        if newer {
            if !self.records.contains_key(head.hk.as_str()) {
                self.records.insert(Arc::from(head.hk.as_str()), BTreeMap::new());
            }
            if let Some(records) = self.records.get_mut(head.hk.as_str()) {
                records.insert(head.sk.as_str().into(), entry);
            }
        }
        Ok(())
    }
}

struct Inner {
    state: State,
    journal: Journal,
}

/// Where a record's newest line is.
enum Location {
    /// In the journal's day file of that name, e.g. 2026-11-04.jsonl.
    Journal { day: String, offset: u64 },
    /// In a DataSet's file.
    File { path: PathBuf, offset: u64 },
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

/// A compacted file's index as it was read, see FileBackend::compacted, the version and offset of each line by its
/// sort key, with the file's size and when it was modified, which say whether it has changed since.
struct CompactedFile {
    path: PathBuf,
    length: u64,
    modified: std::time::SystemTime,
    records: Arc<HashMap<Box<str>, (Version, u64)>>,
}

/// How many compacted files are kept read, enough for puts which go through a few DataSets' days in turn, and fewer on
/// a device, where a day of Tesla snapshots' index is about 1 MB.
#[cfg(not(target_os = "espidf"))]
const COMPACTED_FILES: usize = 8;
#[cfg(target_os = "espidf")]
const COMPACTED_FILES: usize = 2;

impl FileBackend {
    /// Copies the site from, in root, a directory of sites, as the new site to, e.g. for a copy of a profile, see
    /// Store::copy_site: each line of its journal and files with its hash key's site changed to to, and the rest of its
    /// files, its blobs and state, as they are, but for the store's ID, which the copy makes its own. It fails if to's
    /// directory is there already, rather than mixing them.
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
        let mut state = load_state(&target)?;
        if state.id.take().is_some() {
            write_atomically(&target.join(STATE), serde_json::to_string(&state)?.as_bytes())?;
        }
        Ok(())
    }

    /// The store of the site in root, a directory of sites, made if it is not there.
    pub fn open(root: &Path, site: &Id) -> anyhow::Result<FileBackend> {
        let dir = root.join(site.hex());
        fs::create_dir_all(dir.join(JOURNAL))
            .with_context(|| format!("Unable to make the store's directory {}", dir.display()))?;
        let mut state = load_state(&dir)?;
        if state.id.is_none() {
            // Under the lock, and read again, so that two processes opening a new store at once give it one ID.
            let _lock = JournalLock::take(&dir.join(JOURNAL))?;
            state = load_state(&dir)?;
            if state.id.is_none() {
                state.id = Some(new_store_id(&dir)?);
                write_atomically(&dir.join(STATE), serde_json::to_string(&state)?.as_bytes())?;
            }
        }
        let backend = FileBackend {
            dir,
            inner: Mutex::new(Inner {
                state,
                journal: Journal::default(),
            }),
            sync_each_put: std::sync::atomic::AtomicBool::new(true),
            compacted_files: Mutex::new(std::collections::VecDeque::new()),
        };
        // How long opening takes, as the whole journal is read, is logged, being what an application's start waits for,
        // see the design's order of work.
        let started = Instant::now();
        let mut inner = backend.inner.lock().unwrap();
        let indexed = backend.refresh(&mut inner)?;
        info!(
            "Opened the store {} in {:.2?}: {} lines, {} kB, of {} journal days, {} records",
            backend.dir.display(),
            started.elapsed(),
            indexed.lines,
            indexed.bytes / 1024,
            inner.journal.days.len(),
            inner.journal.records.values().map(BTreeMap::len).sum::<usize>(),
        );
        drop(inner);
        Ok(backend)
    }

    /// The site's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn journal_dir(&self) -> PathBuf {
        self.dir.join(JOURNAL)
    }

    /// Indexes what has been added to the journal since it was last read, by this process or another, and reads it all
    /// again if a day has gone, compacted by another process, saying what it read. One which is slow is logged, unless
    /// it is the first reading of the journal, by opening or after this process compacted, which log their own times.
    fn refresh(&self, inner: &mut Inner) -> anyhow::Result<Indexed> {
        let started = Instant::now();
        let first = inner.journal.days.is_empty();
        let mut indexed = Indexed::default();
        let days = journal_days(&self.journal_dir())?;
        if inner.journal.days.iter().any(|(day, _)| !days.contains(day)) {
            inner.journal = Journal::default();
            indexed.again = true;
        }
        for name in days {
            let path = self.journal_dir().join(&name);
            let length = fs::metadata(&path)?.len();
            let from = inner.journal.read_to(&name);
            if length <= from {
                continue;
            }
            let day = inner.journal.day(&name);
            let mut file = File::open(&path)?;
            file.seek(SeekFrom::Start(from))?;
            let mut reader = BufReader::with_capacity(READ_BUFFER, file);
            let mut position = from;
            let mut text = String::new();
            loop {
                text.clear();
                let read = reader.read_line(&mut text)?;
                // A line without its newline is still being written, or was cut short by a crash, so it is left.
                if read == 0 || !text.ends_with('\n') {
                    break;
                }
                let start = position;
                position += read as u64;
                match Head::parse(text.trim_end()) {
                    Ok(head) => {
                        inner.journal.add(&head, day, start)?;
                        indexed.lines += 1;
                    }
                    Err(error) => warn!("Skipping a line of {}: {error:#}", path.display()),
                }
            }
            indexed.bytes += position - from;
            inner.journal.days[day as usize].1 = position;
        }
        if (!first || indexed.again) && started.elapsed() >= SLOW_REFRESH {
            info!(
                "Read the journal of {}{} in {:.2?}: {} lines, {} kB",
                self.dir.display(),
                if indexed.again {
                    " again, compacted by another process,"
                } else {
                    ""
                },
                started.elapsed(),
                indexed.lines,
                indexed.bytes / 1024,
            );
        }
        Ok(indexed)
    }

    /// The version and offset of the record's compacted line, if there is one, and the file it is in, from the file's
    /// index, which is kept, see compacted_files, as a put looks for the record it replaces, and puts are often of one
    /// day's records after another.
    fn compacted(&self, hk: &str, sk: &str) -> anyhow::Result<Option<(Version, PathBuf, u64)>> {
        let path = key::data_set_dir(&self.dir, hk)?.join(key::file_of(sk));
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) if restore(&path) => fs::metadata(&path)?,
            Err(_) => return Ok(None),
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
                read_heads(&path, &mut |head, offset| {
                    let version = Version::new(Micros::parse(&head.v)?, &head.o);
                    records.insert(head.sk.into_boxed_str(), (version, offset));
                    Ok(Flow::Continue)
                })?;
                CompactedFile {
                    path,
                    length,
                    modified,
                    records: Arc::new(records),
                }
            }
        };
        let found = file
            .records
            .get(sk)
            .map(|(version, offset)| (version.clone(), file.path.clone(), *offset));
        files.push_front(file);
        files.truncate(COMPACTED_FILES);
        Ok(found)
    }

    /// The newest version of the record, journal or compacted, tombstones included, and where its line is.
    fn newest(&self, inner: &Inner, hk: &str, sk: &str) -> anyhow::Result<Option<(Version, Location)>> {
        let journal = inner.journal.get(hk, sk).map(|entry| {
            let day = inner.journal.days[entry.day as usize].0.clone();
            (
                inner.journal.version(entry),
                Location::Journal {
                    day,
                    offset: entry.offset,
                },
            )
        });
        let compacted = self
            .compacted(hk, sk)?
            .map(|(version, path, offset)| (version, Location::File { path, offset }));
        Ok(match (journal, compacted) {
            (Some(journal), Some(compacted)) => Some(if journal.0 >= compacted.0 { journal } else { compacted }),
            (journal, compacted) => journal.or(compacted),
        })
    }

    /// The line of the record hk and sk at location, read with reader. A journal day may have been compacted since it
    /// was indexed, by another thread or process, and a compacted file rewritten, in which case the line is found again
    /// in its DataSet's file; None if it is not there either.
    fn read(&self, reader: &mut LineReader, hk: &str, sk: &str, location: &Location) -> anyhow::Result<Option<Line>> {
        let found = match location {
            Location::Journal { day, offset } => reader.read(&self.journal_dir().join(day), *offset)?,
            Location::File { path, offset } => reader.read(path, *offset)?,
        };
        if let Some(line) = found.filter(|line| line.hk == hk && line.sk == sk) {
            return Ok(Some(line));
        }
        reader.files.clear();
        match self.compacted(hk, sk)? {
            Some((_, path, offset)) => Ok(reader
                .read(&path, offset)?
                .filter(|line| line.hk == hk && line.sk == sk)),
            None => Ok(None),
        }
    }

    /// The next sequence number to store with, one after the last given, see State.
    fn next_sequence(inner: &Inner) -> u64 {
        inner.journal.last_sequence.max(inner.state.last_sequence).unwrap_or(0) + 1
    }

    /// The head and offset of every compacted line of the site, and the file it is in, for changes from before the
    /// journal.
    fn all_compacted(&self, visit: &mut dyn FnMut(&Path, Head, u64) -> anyhow::Result<Flow>) -> anyhow::Result<()> {
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
                    read_heads(&path, &mut |head, offset| visit(&path, head, offset))?;
                }
            }
        }
        Ok(())
    }

    fn blob_path(&self, id: &Id) -> PathBuf {
        let hex = id.hex();
        self.dir.join(BLOBS).join(&hex[0..2]).join(hex)
    }

    /// Writes the DataSet's file path again with the journal's lines for it, entries, by sort key, each its day's index
    /// in days and its offset, merged with the lines it has, the newer of any two of one sort key, a line at a time.
    /// The entries are taken one at a time, e.g. straight from the journal's index, so that none are listed. Says how
    /// many lines the file has, and how many of them were the journal's, for the log.
    fn write_merged<'a>(
        &self,
        path: &Path,
        entries: impl Iterator<Item = (&'a str, u32, u64)>,
        days: &[String],
        reader: &mut LineReader,
    ) -> anyhow::Result<(u64, u64)> {
        let (mut written, mut from_journal) = (0, 0);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("tmp");
        let mut out = BufWriter::new(File::create(&temporary)?);
        let mut old = FileLines::new(vec![path.to_path_buf()]).peekable();
        let mut new = entries.peekable();
        let mut read_new = |(sk, day, offset): (&str, u32, u64)| -> anyhow::Result<Line> {
            from_journal += 1;
            let journal = self.journal_dir().join(&days[day as usize]);
            reader
                .read(&journal, offset)?
                .filter(|line| *line.sk == *sk)
                .with_context(|| format!("The line of {sk} is not at {offset} in {}", journal.display()))
        };
        loop {
            if let Some(Err(_)) = old.peek() {
                return Err(old.next().and_then(Result::err).unwrap());
            }
            let old_sk = old
                .peek()
                .and_then(|line| line.as_ref().ok())
                .map(|line| line.sk.as_str());
            let new_sk = new.peek().map(|(sk, _, _)| *sk);
            let line = match (old_sk, new_sk) {
                (None, None) => break,
                (Some(old_sk), Some(new_sk)) if old_sk == new_sk => {
                    let old_line = old.next().unwrap()?;
                    let new_line = read_new(new.next().unwrap())?;
                    if new_line.version()? > old_line.version()? {
                        new_line
                    } else {
                        old_line
                    }
                }
                (Some(old_sk), Some(new_sk)) if old_sk < new_sk => old.next().unwrap()?,
                (Some(_), None) => old.next().unwrap()?,
                _ => read_new(new.next().unwrap())?,
            };
            serde_json::to_writer(&mut out, &line)?;
            out.write_all(b"\n")?;
            written += 1;
        }
        drop(old);
        out.flush()?;
        out.get_ref().sync_all()?;
        drop(out);
        replace(&temporary, path)?;
        Ok((written, from_journal))
    }
}

/// A new store's ID, see State::id, made from where and when it was made, as a created thing's is, see the design's IDs.
fn new_store_id(dir: &Path) -> anyhow::Result<String> {
    // Only ASCII, as IDs are until strings can be normalised, see id.rs.
    let dir: String = dir.to_string_lossy().chars().filter(char::is_ascii).collect();
    Ok(Id::of(&serde_json::json!({
        "store": {"dir": dir, "created": Micros::now().to_text(), "process": std::process::id()},
    }))?
    .to_string())
}

/// The state of the site in dir, the default if it has none yet.
fn load_state(dir: &Path) -> anyhow::Result<State> {
    restore(&dir.join(STATE));
    match fs::read_to_string(dir.join(STATE)) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("The store's {STATE} can not be read")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(error) => Err(error).with_context(|| format!("Unable to read the store's {STATE}")),
    }
}

/// The names of the journal's day files, e.g. 2026-11-04.jsonl, in order.
fn journal_days(journal_dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut days: Vec<String> = fs::read_dir(journal_dir)?
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| name.ends_with(".jsonl"))
        .collect();
    days.sort();
    Ok(days)
}

/// Reads each line of a file, if it is there, with its offset, until visit says Stop.
fn read_texts(path: &Path, visit: &mut dyn FnMut(&str, u64) -> anyhow::Result<Flow>) -> anyhow::Result<()> {
    let file = match open_replaced(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("Unable to read {}", path.display())),
    };
    let mut reader = BufReader::with_capacity(READ_BUFFER, file);
    let mut text = String::new();
    let mut position = 0;
    loop {
        text.clear();
        let read = reader.read_line(&mut text)?;
        if read == 0 {
            return Ok(());
        }
        let start = position;
        position += read as u64;
        if text.trim().is_empty() {
            continue;
        }
        if visit(text.trim_end(), start).with_context(|| format!("In {}", path.display()))? == Flow::Stop {
            return Ok(());
        }
    }
}

/// Reads each line of a file, if it is there, until visit says Stop.
fn read_lines(path: &Path, visit: &mut dyn FnMut(Line) -> anyhow::Result<Flow>) -> anyhow::Result<()> {
    read_texts(path, &mut |text, _| visit(Line::parse(text)?))
}

/// Reads the head of each line of a file, see Head, with its offset, if it is there, until visit says Stop.
fn read_heads(path: &Path, visit: &mut dyn FnMut(Head, u64) -> anyhow::Result<Flow>) -> anyhow::Result<()> {
    read_texts(path, &mut |text, offset| visit(Head::parse(text)?, offset))
}

/// How many files a LineReader keeps open, one, as a device's SD card allows only a few open at once, 4 on the capture
/// devices, and compaction also has the file it merges and the one it writes open: with a file kept open for each
/// journal day, compaction of four days failed with "Too many open files in system" on 13 November 2026.
const MAX_READERS: usize = 1;

/// Reads lines where they are, keeping the file it last opened, see MAX_READERS, as a query or the change feed reads
/// many lines from one file after another.
#[derive(Default)]
struct LineReader {
    files: HashMap<PathBuf, BufReader<File>>,
}

impl LineReader {
    /// The line at offset in the file, None if the file is not there or the offset is at its end.
    fn read(&mut self, path: &Path, offset: u64) -> anyhow::Result<Option<Line>> {
        if !self.files.contains_key(path) {
            if self.files.len() >= MAX_READERS {
                self.files.clear();
            }
            match open_replaced(path) {
                Ok(file) => self
                    .files
                    .insert(path.to_path_buf(), BufReader::with_capacity(READ_BUFFER, file)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error).with_context(|| format!("Unable to read {}", path.display())),
            };
        }
        let Some(reader) = self.files.get_mut(path) else {
            return Ok(None);
        };
        // Relative to where the reader is, so that a line already in its buffer, as the next one asked for usually
        // is, is read from it, rather than the buffer being thrown away and refilled from the file for each line,
        // which made compacting days of 10 second Tesla records take over 20 minutes on a capture device; and a seek
        // forward on FAT goes on from where the file is, rather than from its start.
        let here = reader.stream_position()?;
        reader.seek_relative(offset as i64 - here as i64)?;
        let mut text = String::new();
        reader.read_line(&mut text)?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(
            Line::parse(text.trim_end()).with_context(|| format!("At {offset} in {}", path.display()))?,
        ))
    }
}

/// The lines of files, one after the other, none of a file which is not there.
struct FileLines {
    paths: std::collections::VecDeque<PathBuf>,
    current: Option<(PathBuf, BufReader<File>)>,
    text: String,
}

impl FileLines {
    fn new(paths: Vec<PathBuf>) -> FileLines {
        FileLines {
            paths: paths.into(),
            current: None,
            text: String::new(),
        }
    }
}

impl Iterator for FileLines {
    type Item = anyhow::Result<Line>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let Some((path, reader)) = &mut self.current else {
                let path = self.paths.pop_front()?;
                match open_replaced(&path) {
                    Ok(file) => self.current = Some((path, BufReader::with_capacity(READ_BUFFER, file))),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Some(Err(error).with_context(|| format!("Unable to read {}", path.display())));
                    }
                }
                continue;
            };
            self.text.clear();
            match reader.read_line(&mut self.text) {
                Ok(0) => self.current = None,
                Ok(_) if self.text.trim().is_empty() => {}
                Ok(_) => {
                    return Some(Line::parse(self.text.trim_end()).with_context(|| format!("In {}", path.display())));
                }
                Err(error) => return Some(Err(error).with_context(|| format!("Unable to read {}", path.display()))),
            }
        }
    }
}

/// Sources of lines, each in sort key order.
type Sources<'a> = Vec<Box<dyn Iterator<Item = anyhow::Result<Line>> + 'a>>;

/// Gives visit the newest line of each sort key of sources, in sort key order, until it says Stop, holding only the
/// next line of each.
fn merge(sources: Sources, visit: &mut dyn FnMut(Line) -> anyhow::Result<Flow>) -> anyhow::Result<()> {
    let mut sources: Vec<_> = sources.into_iter().map(Iterator::peekable).collect();
    loop {
        let mut smallest: Option<String> = None;
        for source in &mut sources {
            match source.peek() {
                Some(Err(_)) => return Err(source.next().and_then(Result::err).unwrap()),
                Some(Ok(line)) if smallest.as_ref().is_none_or(|smallest| line.sk < *smallest) => {
                    smallest = Some(line.sk.clone());
                }
                _ => {}
            }
        }
        let Some(sk) = smallest else {
            return Ok(());
        };
        let mut newest: Option<Line> = None;
        for source in &mut sources {
            while source
                .peek()
                .is_some_and(|line| line.as_ref().is_ok_and(|line| line.sk == sk))
            {
                let line = source.next().unwrap()?;
                newest = Some(match newest {
                    Some(existing) if existing.version()? >= line.version()? => existing,
                    _ => line,
                });
            }
        }
        if let Some(line) = newest
            && visit(line)? == Flow::Stop
        {
            return Ok(());
        }
    }
}

/// A change found for the feed, see changes, ordered by its sequence number alone, so that a heap keeps the first.
struct Pending {
    n: u64,
    hk: Arc<str>,
    sk: Box<str>,
    location: Location,
}

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        self.n == other.n
    }
}

impl Eq for Pending {}

impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Pending {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.n.cmp(&other.n)
    }
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
    replace(&temporary, path)
}

/// Puts the file temporary in place of path. A rename replaces a file on macOS and Linux, but not on FAT under
/// ESP-IDF, where it fails with "File exists", so there the old file is first moved aside, see aside, and deleted
/// once the new one is in place. A crash between the two renames leaves only the old file, aside, which restore puts
/// back, so that the file is never lost; the work being done, e.g. compaction, is then done again, as its journal is
/// only deleted once it has finished.
fn replace(temporary: &Path, path: &Path) -> anyhow::Result<()> {
    let context = || format!("Unable to write {}", path.display());
    match fs::rename(temporary, path) {
        Ok(()) => Ok(()),
        Err(_) if path.exists() => replace_by_moving_aside(temporary, path).with_context(context),
        Err(error) => Err(error).with_context(context),
    }
}

/// Puts temporary in place of path where a rename does not replace a file, see replace.
fn replace_by_moving_aside(temporary: &Path, path: &Path) -> std::io::Result<()> {
    let old = aside(path);
    if old.exists() {
        fs::remove_file(&old)?;
    }
    fs::rename(path, &old)?;
    fs::rename(temporary, path)?;
    if let Err(error) = fs::remove_file(&old) {
        warn!("Unable to delete {}, {error:#}", old.display());
    }
    Ok(())
}

/// Where replace moves a file aside while it puts another in its place, e.g. records.old for records.jsonl, which is
/// not read as a DataSet's file, as only .jsonl files are.
fn aside(path: &Path) -> PathBuf {
    path.with_extension("old")
}

/// Puts back a file which a crash in replace left aside, if path is not there, saying whether it did, for each place a
/// file which may have been replaced is read and found missing.
fn restore(path: &Path) -> bool {
    let old = aside(path);
    if path.exists() || !old.exists() {
        return false;
    }
    match fs::rename(&old, path) {
        Ok(()) => {
            warn!("Put back {}, which was left aside as it was replaced", path.display());
            true
        }
        Err(error) => {
            warn!("Unable to put back {}, {error:#}", path.display());
            false
        }
    }
}

/// Opens a file which may have been replaced, see restore.
fn open_replaced(path: &Path) -> std::io::Result<File> {
    match File::open(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && restore(path) => File::open(path),
        result => result,
    }
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
    fn store_id(&self) -> String {
        self.inner.lock().unwrap().state.id.clone().unwrap_or_default()
    }

    fn put(&self, item: &Item, from: Option<&str>) -> anyhow::Result<Put> {
        if key::site_of(&item.hk)?.hex() != self.dir.file_name().and_then(|name| name.to_str()).unwrap_or_default() {
            bail!("{} is not a hash key of this store's site", item.hk);
        }
        key::data_set_dir(&self.dir, &item.hk)?;
        let _lock = JournalLock::take(&self.journal_dir())?;
        let mut inner = self.inner.lock().unwrap();
        self.refresh(&mut inner)?;
        if let Some((existing, _)) = self.newest(&inner, &item.hk, &item.sk)?
            && existing >= item.version
        {
            return Ok(Put::Older);
        }
        let stored = Micros::now();
        let line = Line::new(item, Self::next_sequence(&inner), stored, from);
        let day = format!("{}.jsonl", stored.date()?);
        let path = self.journal_dir().join(&day);
        let mut text = serde_json::to_string(&line)?;
        text.push('\n');
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        let before = file.metadata()?.len();
        file.write_all(text.as_bytes())?;
        if self.sync_each_put.load(std::sync::atomic::Ordering::Relaxed) {
            file.sync_data()?;
        }
        drop(file);
        if inner.journal.read_to(&day) == before {
            // Nothing else was added since it was read, so the line is indexed, and read past.
            let index = inner.journal.day(&day);
            inner.journal.add(&Head::of(&line), index, before)?;
            inner.journal.days[index as usize].1 = before + text.len() as u64;
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
                // Windows flushes a file only through a handle which may write to it, and refuses a read-only one.
                OpenOptions::new().append(true).open(entry.path())?.sync_all()?;
            }
        }
        Ok(())
    }

    fn get(&self, hk: &str, sk: &str) -> anyhow::Result<Option<Item>> {
        let location = {
            let mut inner = self.inner.lock().unwrap();
            self.refresh(&mut inner)?;
            self.newest(&inner, hk, sk)?
        };
        let Some((_, location)) = location else {
            return Ok(None);
        };
        match self.read(&mut LineReader::default(), hk, sk, &location)? {
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
        if range.start >= range.end {
            return Ok(());
        }
        let dir = key::data_set_dir(&self.dir, hk)?;
        let mut days = Vec::new();
        for year in sorted_entries(&dir)? {
            for month in sorted_entries(&dir.join(&year))? {
                for day_file in sorted_entries(&dir.join(&year).join(&month))? {
                    let Some(day) = day_file.strip_suffix(".jsonl") else {
                        continue;
                    };
                    if year.len() == 4 && day_in_range(&format!("{year}-{month}-{day}"), &range) {
                        days.push(dir.join(&year).join(&month).join(&day_file));
                    }
                }
            }
        }
        // The journal's entries in the range, without their lines, which are read as the merge gets to them, not under
        // the lock, as visit may use the store.
        let journal: Vec<(Box<str>, Location)> = {
            let mut inner = self.inner.lock().unwrap();
            self.refresh(&mut inner)?;
            match inner.journal.records.get(hk) {
                Some(records) => records
                    .range::<str, _>((Bound::Included(range.start), Bound::Excluded(range.end)))
                    .map(|(sk, entry)| {
                        let day = inner.journal.days[entry.day as usize].0.clone();
                        (
                            sk.clone(),
                            Location::Journal {
                                day,
                                offset: entry.offset,
                            },
                        )
                    })
                    .collect(),
                None => Vec::new(),
            }
        };
        let mut reader = LineReader::default();
        let journal = journal
            .into_iter()
            .filter_map(move |(sk, location)| self.read(&mut reader, hk, &sk, &location).transpose());
        // A DataSet's records.jsonl and its day files each hold their lines in sort key order, but one's may sort
        // between the other's, e.g. 2026-11 between the last of October's days and the first of November's.
        let sources: Sources = vec![
            Box::new(FileLines::new(vec![dir.join(RECORDS)])),
            Box::new(FileLines::new(days)),
            Box::new(journal),
        ];
        merge(sources, &mut |line| {
            if line.hk != hk || line.sk.as_str() < range.start || line.sk.as_str() >= range.end || line.a.is_none() {
                return Ok(Flow::Continue);
            }
            visit(line.item()?)
        })
    }

    fn changes(
        &self,
        since: Cursor,
        limit: usize,
        for_peer: Option<&str>,
        visit: &mut dyn FnMut(Change) -> anyhow::Result<()>,
    ) -> anyhow::Result<Cursor> {
        if limit == 0 {
            return Ok(since);
        }
        // The first limit changes after since, by sequence number, from the journal's index and, if since is before the
        // end of what was compacted, from the DataSets' files, which keep each line's sequence number, read without
        // their attributes. Superseded lines are not in either, so a record's newest version is given, once, unless
        // a newer one was put after it was compacted, which is given as well, harmlessly.
        let mut first: BinaryHeap<Pending> = BinaryHeap::new();
        let offer = |first: &mut BinaryHeap<Pending>, n: u64, make: &mut dyn FnMut() -> Pending| {
            if n <= since.0 || (first.len() == limit && first.peek().is_some_and(|last| n >= last.n)) {
                return;
            }
            first.push(make());
            if first.len() > limit {
                first.pop();
            }
        };
        let compacted_through = {
            let mut inner = self.inner.lock().unwrap();
            self.refresh(&mut inner)?;
            for (hk, records) in &inner.journal.records {
                for (sk, entry) in records {
                    offer(&mut first, entry.n, &mut || Pending {
                        n: entry.n,
                        hk: hk.clone(),
                        sk: sk.clone(),
                        location: Location::Journal {
                            day: inner.journal.days[entry.day as usize].0.clone(),
                            offset: entry.offset,
                        },
                    });
                }
            }
            inner.state.compacted_through
        };
        if compacted_through.is_some_and(|through| through > since.0) {
            self.all_compacted(&mut |path, head, offset| {
                offer(&mut first, head.n, &mut || Pending {
                    n: head.n,
                    hk: Arc::from(head.hk.as_str()),
                    sk: head.sk.as_str().into(),
                    location: Location::File {
                        path: path.to_path_buf(),
                        offset,
                    },
                });
                Ok(Flow::Continue)
            })?;
        }
        let mut cursor = since;
        let mut reader = LineReader::default();
        for pending in first.into_sorted_vec() {
            cursor = Cursor(pending.n);
            let Some(line) = self.read(&mut reader, &pending.hk, &pending.sk, &pending.location)? else {
                continue;
            };
            if for_peer.is_some_and(|peer| line.from.as_deref() == Some(peer) || line.o == peer) {
                continue;
            }
            visit(Change {
                item: line.item()?,
                sequence: line.n,
                stored: line.stored()?,
                from: line.from.clone(),
            })?;
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
                    .map(|hk| hk.to_string()),
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
        self.refresh(&mut inner)?;
        let days: Vec<String> = journal_days(&self.journal_dir())?
            .into_iter()
            .filter(|name| name.as_str() < format!("{before}.jsonl").as_str())
            .collect();
        if days.is_empty() {
            return Ok(());
        }
        // How long compaction takes is logged, as puts, gets and queries wait while it runs, see the design's order of
        // work.
        let started = Instant::now();
        let (mut files, mut written, mut from_journal) = (0u64, 0u64, 0u64);
        // Another process may have compacted since this one opened the store.
        let mut state = load_state(&self.dir)?;
        let compacting: Vec<u32> = days.iter().map(|day| inner.journal.day(day)).collect();
        let day_names: Vec<String> = inner.journal.days.iter().map(|(day, _)| day.clone()).collect();
        let in_days = |entry: &Entry| compacting.contains(&entry.day);
        let mut latest = state.compacted_through;
        let mut reader = LineReader::default();
        let count = inner.journal.records.len();
        // The journal's newest line of each record in those days is merged into the file it goes in, a DataSet at a
        // time and a file at a time, streamed from the journal's index in sort key order, as the files' lines are, so
        // that compaction needs little memory beyond the index, however many records it folds: listing them all first
        // ran a capture device out of memory. A record whose newest line is in a later day is left to that day's
        // compaction.
        for (index, (hk, records)) in inner.journal.records.iter().enumerate() {
            bruce_util_progress::report_count(progress, index, count, "DataSets compacted");
            let dir = key::data_set_dir(&self.dir, hk)?;
            // The dates of the records being compacted, each a run in sort key order, as the sort keys of a date all
            // start with it, and whether any have no date, which go in the DataSet's records file, wherever they sort.
            let mut dates: Vec<&str> = Vec::new();
            let mut undated = false;
            for (sk, entry) in records.iter().filter(|(_, entry)| in_days(entry)) {
                latest = latest.max(Some(entry.n));
                match key::sort_key_date(sk) {
                    Some(_) if dates.last() == Some(&&sk[..10]) => {}
                    Some(_) => dates.push(&sk[..10]),
                    None => undated = true,
                }
            }
            for date in dates {
                // Stopping between files leaves each file whole, and the journal, which is only deleted at the end, to
                // be compacted again, which gives the same files.
                progress.check()?;
                let entries = records
                    .range::<str, _>((Bound::Included(date), Bound::Unbounded))
                    .take_while(|(sk, _)| sk.starts_with(date))
                    .filter(|(_, entry)| in_days(entry))
                    .map(|(sk, entry)| (&**sk, entry.day, entry.offset));
                let (lines, new) =
                    self.write_merged(&dir.join(key::file_of(date)), entries, &day_names, &mut reader)?;
                (files, written, from_journal) = (files + 1, written + lines, from_journal + new);
            }
            if undated {
                progress.check()?;
                let entries = records
                    .iter()
                    .filter(|(sk, entry)| in_days(entry) && key::sort_key_date(sk).is_none())
                    .map(|(sk, entry)| (&**sk, entry.day, entry.offset));
                let (lines, new) = self.write_merged(&dir.join(RECORDS), entries, &day_names, &mut reader)?;
                (files, written, from_journal) = (files + 1, written + lines, from_journal + new);
            }
        }
        drop(reader);
        let merged = started.elapsed();
        state = State {
            id: state.id.or(inner.state.id.clone()),
            last_sequence: inner.journal.last_sequence.max(state.last_sequence),
            compacted_through: latest,
        };
        write_atomically(&self.dir.join(STATE), serde_json::to_string(&state)?.as_bytes())?;
        inner.state = state;
        for day in &days {
            fs::remove_file(self.journal_dir().join(day))?;
        }
        inner.journal = Journal::default();
        // The files written may not look changed, as FAT keeps their times to 2 seconds.
        self.compacted_files.lock().unwrap().clear();
        let indexed = self.refresh(&mut inner)?;
        info!(
            "Compacted {} journal days of {} in {:.2?}: {from_journal} records merged into {files} files, {written} \
             lines, in {merged:.2?}, and {} journal lines indexed again",
            days.len(),
            self.dir.display(),
            started.elapsed(),
            indexed.lines,
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own for a test, deleted first in case an earlier run left it.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bruce-db-file-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// FAT under ESP-IDF does not rename over a file, so replace moves the old one aside, which this does as it would
    /// there, as a rename on the Mac always replaces.
    #[test]
    fn a_file_is_replaced_by_moving_the_old_one_aside() {
        let dir = test_dir("replace");
        let (path, temporary) = (dir.join("records.jsonl"), dir.join("records.tmp"));
        fs::write(&path, "old\n").unwrap();
        fs::write(&temporary, "new\n").unwrap();
        replace_by_moving_aside(&temporary, &path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
        assert!(!temporary.exists());
        assert!(
            !aside(&path).exists(),
            "the old file is deleted once the new one is in place"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A crash between replace's two renames leaves only the old file, aside, which is put back when the file is next
    /// read, so that nothing is lost.
    #[test]
    fn a_file_left_aside_by_a_crash_is_put_back_when_read() {
        let dir = test_dir("restore");
        let path = dir.join("records.jsonl");
        fs::write(aside(&path), "old\n").unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut open_replaced(&path).unwrap(), &mut text).unwrap();
        assert_eq!(text, "old\n");
        assert!(!aside(&path).exists());
        assert!(!restore(&path), "nothing more to put back");
        fs::remove_dir_all(&dir).unwrap();
    }
}
