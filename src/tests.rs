//! The store's behaviour through its public interface, on the file backend, in temporary directories.

use std::path::PathBuf;

use serde_json::json;

use crate::*;

/// A directory of its own for a test, removed when it is dropped.
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!("bruce-db-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TestDir(dir)
    }

    fn location(&self) -> String {
        format!("file://{}", self.0.display())
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The cursor a page of the change feed gives, for tests of a peer within the journal kept.
trait NextCursor {
    fn cursor(self) -> Cursor;
}

impl NextCursor for Feed {
    fn cursor(self) -> Cursor {
        match self {
            Feed::Next(cursor) => cursor,
            Feed::TooFarBehind => panic!("too far behind"),
        }
    }
}

fn domain() -> Id {
    Id::of(&json!({"domain": "test"})).unwrap()
}

fn version(seconds: i64, origin: &str) -> Version {
    Version::new(Micros(seconds * 1_000_000), origin)
}

fn open(dir: &TestDir) -> Store {
    Store::open(&dir.location(), &domain()).unwrap()
}

#[test]
fn the_newest_version_wins_and_of_equal_times_the_lowest_origin() {
    let dir = TestDir::new("newest");
    let store = open(&dir);
    let hk = store
        .hash_key("climate.period", &[Part::from("dev1"), Part::from("temperature")])
        .unwrap();
    let sk = "2026-11-04T07:17:10Z";
    assert_eq!(
        store
            .put(&Item::new(&hk, sk, version(10, "b"), json!({"value": 1})))
            .unwrap(),
        Put::Stored
    );
    assert_eq!(
        store
            .put(&Item::new(&hk, sk, version(9, "a"), json!({"value": 2})))
            .unwrap(),
        Put::Older
    );
    assert_eq!(
        store
            .put(&Item::new(&hk, sk, version(10, "b"), json!({"value": 3})))
            .unwrap(),
        Put::Older,
        "the same version again"
    );
    assert_eq!(
        store
            .put(&Item::new(&hk, sk, version(10, "a"), json!({"value": 4})))
            .unwrap(),
        Put::Stored
    );
    assert_eq!(
        store
            .put(&Item::new(&hk, sk, version(10, "c"), json!({"value": 5})))
            .unwrap(),
        Put::Older
    );
    assert_eq!(
        store.get(&hk, sk).unwrap().unwrap().attributes,
        Some(json!({"value": 4}))
    );
    assert_eq!(
        store
            .put(&Item::new(&hk, sk, version(11, "z"), json!({"value": 6})))
            .unwrap(),
        Put::Stored
    );
    assert_eq!(
        store.get(&hk, sk).unwrap().unwrap().attributes,
        Some(json!({"value": 6}))
    );
}

/// Puts which are not synced one by one, as in a bulk load, are synced together afterwards, which on Windows needs each
/// journal file opened for writing.
#[test]
fn unsynced_puts_are_synced_afterwards() {
    let dir = TestDir::new("sync");
    let store = open(&dir);
    store.set_sync(false);
    let hk = store.hash_key("octopus.bill", &[]).unwrap();
    store
        .put(&Item::new(&hk, "2026-10-01#1", version(1, "app"), json!({"total": 1})))
        .unwrap();
    store.sync().unwrap();
    store.set_sync(true);
    assert_eq!(
        store.get(&hk, "2026-10-01#1").unwrap().unwrap().attributes,
        Some(json!({"total": 1}))
    );
}

#[test]
fn a_tombstone_deletes_a_record() {
    let dir = TestDir::new("tombstone");
    let store = open(&dir);
    let hk = store.hash_key("octopus.bill", &[]).unwrap();
    store
        .put(&Item::new(&hk, "2026-10-01#1", version(1, "app"), json!({"total": 1})))
        .unwrap();
    store
        .put(&Item::tombstone(&hk, "2026-10-01#1", version(2, "app")))
        .unwrap();
    assert_eq!(store.get(&hk, "2026-10-01#1").unwrap(), None);
    assert!(store.all(&hk).unwrap().is_empty());
}

#[test]
fn a_query_finds_a_range_in_order_from_the_journal_and_compacted_files_alike() {
    let dir = TestDir::new("query");
    let store = open(&dir);
    let hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
    for (sk, value) in [
        ("2026-11-03T23:59:50Z", 1),
        ("2026-11-04T00:00:00Z", 2),
        ("2026-11-04T12:00:00Z", 3),
        ("2026-11-05T00:00:00Z", 4),
    ] {
        store
            .put(&Item::new(&hk, sk, version(1, "app"), json!({"value": value})))
            .unwrap();
    }
    store
        .put(&Item::new(
            &hk,
            "#fetched",
            version(1, "app"),
            json!({"complete": true}),
        ))
        .unwrap();
    let values = |store: &Store, range: std::ops::Range<&str>| -> Vec<i64> {
        store
            .query_all(&hk, range)
            .unwrap()
            .iter()
            .filter_map(|item| item.attributes.as_ref()?.get("value")?.as_i64())
            .collect()
    };
    assert_eq!(values(&store, "2026-11-04".."2026-11-05"), vec![2, 3]);
    assert_eq!(store.all(&hk).unwrap().len(), 5);
    assert_eq!(
        store.query_all(&hk, "#".."0").unwrap().len(),
        1,
        "the DataSet's own records sort before the times"
    );

    // Compacting moves them into the DataSet's files, and nothing changes.
    let tomorrow = Micros::now().date().unwrap().next_day().unwrap();
    store.compact(tomorrow).unwrap();
    assert!(
        std::fs::read_dir(dir.0.join(domain().hex()).join("journal"))
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".jsonl"))
    );
    assert_eq!(values(&store, "2026-11-04".."2026-11-05"), vec![2, 3]);
    assert!(
        dir.0
            .join(domain().hex())
            .join("tesla.snapshot/gw/2026/11/04.jsonl")
            .exists()
    );
    assert!(
        dir.0
            .join(domain().hex())
            .join("tesla.snapshot/gw/records.jsonl")
            .exists()
    );

    // A newer version in the journal still wins over a compacted one, and an older one is still refused.
    store
        .put(&Item::new(
            &hk,
            "2026-11-04T12:00:00Z",
            version(2, "app"),
            json!({"value": 30}),
        ))
        .unwrap();
    assert_eq!(
        store
            .put(&Item::new(
                &hk,
                "2026-11-04T00:00:00Z",
                version(0, "app"),
                json!({"value": 0})
            ))
            .unwrap(),
        Put::Older
    );
    assert_eq!(values(&open(&dir), "2026-11-04".."2026-11-05"), vec![2, 30]);
}

#[test]
fn changes_are_given_in_the_order_stored_from_a_cursor_and_not_back_to_their_peer() {
    let dir = TestDir::new("changes");
    let store = open(&dir);
    let hk = store.hash_key("app.poll", &[Part::from("octopus")]).unwrap();
    store
        .put(&Item::new(
            &hk,
            "2026-11-04T01:00:00Z",
            version(1, "app"),
            json!({"n": 1}),
        ))
        .unwrap();
    store
        .put_from(
            &Item::new(&hk, "2026-11-04T02:00:00Z", version(2, "dev1"), json!({"n": 2})),
            "dev1",
        )
        .unwrap();
    store
        .put(&Item::new(
            &hk,
            "2026-11-04T03:00:00Z",
            version(3, "app"),
            json!({"n": 3}),
        ))
        .unwrap();

    let collect = |store: &Store, since: Cursor, limit: usize, peer: Option<&str>| {
        let mut found = Vec::new();
        let cursor = store
            .changes(since, limit, peer, &mut |change| {
                found.push(change.item.attributes.unwrap()["n"].as_i64().unwrap());
                Ok(())
            })
            .unwrap()
            .cursor();
        (found, cursor)
    };
    let (all, end) = collect(&store, Cursor::START, 100, None);
    assert_eq!(all, vec![1, 2, 3]);
    let (first, cursor) = collect(&store, Cursor::START, 1, None);
    assert_eq!(first, vec![1]);
    assert_eq!(
        cursor,
        Cursor(1),
        "the cursor is the sequence number of the last change given"
    );
    assert_eq!(end, Cursor(3));
    assert_eq!(collect(&store, cursor, 100, None).0, vec![2, 3]);
    assert_eq!(
        collect(&store, Cursor::START, 100, Some("dev1")).0,
        vec![1, 3],
        "not sent back to the peer it came from"
    );
    assert!(collect(&store, end, 100, None).0.is_empty());

    // After compaction, a peer starting afresh still gets everything, and one up to date nothing more.
    store
        .compact(Micros::now().date().unwrap().next_day().unwrap())
        .unwrap();
    assert_eq!(collect(&store, Cursor::START, 100, None).0, vec![1, 2, 3]);
    assert!(collect(&store, end, 100, None).0.is_empty());
    store
        .put(&Item::new(
            &hk,
            "2026-11-04T04:00:00Z",
            version(4, "app"),
            json!({"n": 4}),
        ))
        .unwrap();
    let (after, later) = collect(&open(&dir), end, 100, None);
    assert_eq!(after, vec![4]);
    assert!(
        later > end,
        "times stored go forward after the journal was compacted and the store opened again"
    );
}

#[test]
fn a_second_process_sees_what_the_first_puts() {
    let dir = TestDir::new("processes");
    let first = open(&dir);
    let second = open(&dir);
    let hk = first.hash_key("octopus.account", &[]).unwrap();
    first
        .put(&Item::new(&hk, "-", version(1, "app"), json!({"n": 1})))
        .unwrap();
    assert_eq!(second.get(&hk, "-").unwrap().unwrap().attributes, Some(json!({"n": 1})));
    assert_eq!(
        second
            .put(&Item::new(&hk, "-", version(1, "app"), json!({"n": 1})))
            .unwrap(),
        Put::Older
    );
}

#[test]
fn data_sets_are_listed_by_prefix_with_their_ids() {
    let dir = TestDir::new("data-sets");
    let store = open(&dir);
    let meter = Id::of(&json!({"meter": "1"})).unwrap();
    let readings = store.hash_key("octopus.reading", &[Part::from(&meter)]).unwrap();
    let climate = store
        .hash_key("climate.period", &[Part::from("dev1"), Part::from("temperature")])
        .unwrap();
    store
        .put(&Item::new(
            &readings,
            "2026-11-04T00:00:00Z",
            version(1, "app"),
            json!({}),
        ))
        .unwrap();
    store
        .put(&Item::new(
            &climate,
            "2026-11-04T00:00:00Z",
            version(1, "app"),
            json!({}),
        ))
        .unwrap();
    assert_eq!(
        store.data_sets(&format!("{}/octopus.", domain())).unwrap(),
        vec![readings.clone()]
    );
    store
        .compact(Micros::now().date().unwrap().next_day().unwrap())
        .unwrap();
    assert_eq!(
        store.data_sets(&format!("{}/", domain())).unwrap(),
        vec![climate, readings]
    );
}

#[test]
fn a_blob_is_kept_by_the_hash_of_its_bytes() {
    let dir = TestDir::new("blobs");
    let store = open(&dir);
    let id = store.put_blob(b"%PDF-1.4 a bill").unwrap();
    assert_eq!(id, Id::of_bytes(b"%PDF-1.4 a bill"));
    assert_eq!(store.put_blob(b"%PDF-1.4 a bill").unwrap(), id);
    assert_eq!(store.get_blob(&id).unwrap().as_deref(), Some(&b"%PDF-1.4 a bill"[..]));
    assert_eq!(store.get_blob(&Id::of_bytes(b"other")).unwrap(), None);
}

#[test]
fn a_record_of_another_site_or_a_bad_hash_key_is_refused() {
    let dir = TestDir::new("refused");
    let store = open(&dir);
    let other = Id::of(&json!({"domain": "other"})).unwrap();
    let hk = hash_key(&other, "climate.period", &[]).unwrap();
    assert!(store.put(&Item::new(&hk, "-", version(1, "app"), json!({}))).is_err());
    assert!(
        store
            .put(&Item::new(
                &format!("{}/Bad Kind", domain()),
                "-",
                version(1, "app"),
                json!({})
            ))
            .is_err()
    );
}

/// Compaction streams each file's records from the journal's index, a date's sort keys being a run of it, so undated
/// sort keys which sort before and after the dated ones must still all reach the records file, and each date its own,
/// merged with what the files already hold.
#[test]
fn compaction_puts_dated_and_undated_records_in_their_files_wherever_they_sort() {
    let dir = TestDir::new("compact-runs");
    let store = open(&dir);
    let hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
    let put = |sk: &str, seconds: i64, value: i64| {
        store
            .put(&Item::new(&hk, sk, version(seconds, "app"), json!({ "value": value })))
            .unwrap();
    };
    let tomorrow = Micros::now().date().unwrap().next_day().unwrap();
    // A first compaction, so that the second merges into files already there.
    put("2026-11-01T00:00:00Z", 1, 1);
    put("#settings", 1, 1);
    store.compact(tomorrow).unwrap();
    put("#settings", 2, 2);
    put("2026-11-01T00:00:10Z", 2, 2);
    put("2026-11-02T00:00:00Z", 2, 2);
    put("2026-11-02T12:00:00Z", 2, 2);
    put("2026-11-03T00:00:00Z", 2, 2);
    put("zz-after-the-dates", 2, 2);
    put("-", 2, 2);
    store.compact(tomorrow).unwrap();

    let reopened = open(&dir);
    let sks: Vec<String> = reopened.all(&hk).unwrap().into_iter().map(|item| item.sk).collect();
    assert_eq!(
        sks,
        [
            "#settings",
            "-",
            "2026-11-01T00:00:00Z",
            "2026-11-01T00:00:10Z",
            "2026-11-02T00:00:00Z",
            "2026-11-02T12:00:00Z",
            "2026-11-03T00:00:00Z",
            "zz-after-the-dates"
        ]
    );
    assert_eq!(
        reopened.get(&hk, "#settings").unwrap().unwrap().attributes,
        Some(json!({ "value": 2 })),
        "the newer version wins over the one compacted before"
    );
}

#[test]
fn a_cancelled_compaction_loses_nothing_and_can_be_done_again() {
    let dir = TestDir::new("cancelled");
    let store = open(&dir);
    let hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
    for day in 1..=3 {
        store
            .put(&Item::new(
                &hk,
                &format!("2026-11-0{day}T00:00:00Z"),
                version(day, "app"),
                json!({"day": day}),
            ))
            .unwrap();
    }
    let tomorrow = Micros::now().date().unwrap().next_day().unwrap();
    let cancel = progress::Cancel::new();
    cancel.cancel();
    let mut cancelled = progress::Reporter::new(|_: &str| {}).with_cancel(cancel);
    let error = store.compact_with_progress(tomorrow, &mut cancelled).unwrap_err();
    assert!(error.is::<progress::Cancelled>());
    assert_eq!(store.all(&hk).unwrap().len(), 3);
    assert_eq!(open(&dir).all(&hk).unwrap().len(), 3, "the journal is still there");

    let mut shown = Vec::new();
    store
        .compact_with_progress(
            tomorrow,
            &mut progress::Reporter::new(|message: &str| shown.push(message.to_string())),
        )
        .unwrap();
    assert_eq!(store.all(&hk).unwrap().len(), 3);
    assert_eq!(shown.last().map(String::as_str), Some("1 of 1 DataSets compacted"));
}

#[test]
fn a_copied_site_has_the_same_records_under_its_own_hash_keys() {
    let dir = TestDir::new("copy-domain");
    let store = open(&dir);
    let hk = store.hash_key("climate.period", &[Part::from("dev1")]).unwrap();
    store
        .put(&Item::new(
            &hk,
            "2026-11-01T00:00:00Z",
            version(1, "app"),
            json!({"n": 1}),
        ))
        .unwrap();
    store
        .compact(Micros::now().date().unwrap().next_day().unwrap())
        .unwrap();
    store
        .put(&Item::new(
            &hk,
            "2026-11-02T00:00:00Z",
            version(2, "app"),
            json!({"n": 2}),
        ))
        .unwrap();
    let blob = store.put_blob(b"a bill").unwrap();

    let copy = Id::of(&json!({"domain": "copy"})).unwrap();
    Store::copy_domain(&dir.location(), &domain(), &copy).unwrap();
    let copied = Store::open(&dir.location(), &copy).unwrap();
    let copied_hk = copied.hash_key("climate.period", &[Part::from("dev1")]).unwrap();
    assert_eq!(
        copied.all(&copied_hk).unwrap().len(),
        2,
        "from the compacted files and the journal"
    );
    assert_eq!(copied.get_blob(&blob).unwrap().as_deref(), Some(&b"a bill"[..]));
    assert!(
        Store::copy_domain(&dir.location(), &domain(), &copy).is_err(),
        "not over a domain which is there"
    );
    assert_eq!(store.all(&hk).unwrap().len(), 2, "the original is as it was");
}

#[test]
fn a_query_merges_records_whose_sort_keys_are_not_dates_between_the_days() {
    let dir = TestDir::new("interleaved");
    let store = open(&dir);
    let hk = store.hash_key("octopus.consumption", &[]).unwrap();
    for (sk, seconds) in [
        ("2026-10-31T23:30:00Z", 1),
        ("2026-11", 2),
        ("2026-11-01T00:00:00Z", 3),
        ("#fetched/2026-11", 4),
    ] {
        store
            .put(&Item::new(&hk, sk, version(seconds, "app"), json!({"sk": sk})))
            .unwrap();
    }
    let order = |store: &Store| -> Vec<String> { store.all(&hk).unwrap().into_iter().map(|item| item.sk).collect() };
    let expected = vec![
        "#fetched/2026-11",
        "2026-10-31T23:30:00Z",
        "2026-11",
        "2026-11-01T00:00:00Z",
    ];
    assert_eq!(order(&store), expected);
    store
        .compact(Micros::now().date().unwrap().next_day().unwrap())
        .unwrap();
    assert_eq!(
        order(&store),
        expected,
        "from records.jsonl and the day files, in order"
    );
    // A newer version in the journal of one in the middle, and a query stopped part way.
    store
        .put(&Item::new(&hk, "2026-11", version(9, "app"), json!({"sk": "newer"})))
        .unwrap();
    assert_eq!(order(&store), expected);
    assert_eq!(
        store.get(&hk, "2026-11").unwrap().unwrap().attributes,
        Some(json!({"sk": "newer"}))
    );
    let mut first_two = Vec::new();
    store
        .query(&hk, ALL, &mut |item| {
            first_two.push(item.sk);
            Ok(if first_two.len() == 2 {
                Flow::Stop
            } else {
                Flow::Continue
            })
        })
        .unwrap();
    assert_eq!(first_two, expected[..2]);
}

#[test]
fn changes_page_one_at_a_time_through_compacted_files_and_the_journal() {
    let dir = TestDir::new("paging");
    let store = open(&dir);
    let hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
    let put = |store: &Store, sk: &str, seconds: i64| {
        store
            .put(&Item::new(&hk, sk, version(seconds, "dev1"), json!({"s": seconds})))
            .unwrap();
    };
    put(&store, "2026-11-01T00:00:00Z", 1);
    put(&store, "2026-11-02T00:00:00Z", 2);
    put(&store, "-", 3);
    store
        .compact(Micros::now().date().unwrap().next_day().unwrap())
        .unwrap();
    put(&store, "2026-11-03T00:00:00Z", 4);
    // A newer version of a compacted record, which is given as well.
    put(&store, "2026-11-01T00:00:00Z", 5);
    let mut found = Vec::new();
    let mut cursor = Cursor::START;
    loop {
        let next = store
            .changes(cursor, 1, None, &mut |change| {
                found.push((change.sequence, change.item.attributes.unwrap()["s"].as_i64().unwrap()));
                Ok(())
            })
            .unwrap()
            .cursor();
        if next == cursor {
            break;
        }
        cursor = next;
    }
    assert_eq!(
        found,
        vec![(1, 1), (2, 2), (3, 3), (4, 4), (5, 5)],
        "the first's compacted version is given before it was superseded, and its newer one after"
    );
}

#[test]
fn a_store_keeps_its_id_and_a_copy_has_its_own() {
    let dir = TestDir::new("store-id");
    let id = open(&dir).store_id();
    assert!(is_id(&id));
    assert_eq!(open(&dir).store_id(), id, "kept when opened again");
    let copy = Id::of(&json!({"domain": "copy"})).unwrap();
    Store::copy_domain(&dir.location(), &domain(), &copy).unwrap();
    let copied = Store::open(&dir.location(), &copy).unwrap().store_id();
    assert!(is_id(&copied));
    assert_ne!(copied, id);
}

#[test]
fn a_change_goes_over_the_feed_as_a_line_and_comes_back_the_same() {
    let dir = TestDir::new("wire");
    let store = open(&dir);
    let hk = store.hash_key("tesla.snapshot", &[Part::from("gateway")]).unwrap();
    store
        .put_from(
            &Item::new(&hk, "2026-11-04T00:00:00Z", version(1, "dev1"), json!({"s": 1})),
            "dev1",
        )
        .unwrap();
    let mut changes = Vec::new();
    store
        .changes(Cursor::START, 10, None, &mut |change| {
            changes.push(change);
            Ok(())
        })
        .unwrap()
        .cursor();
    let json = changes[0].to_json().unwrap();
    assert_eq!(json["hk"], json!(hk));
    assert_eq!(json["from"], json!("dev1"));
    assert_eq!(Change::from_json(json).unwrap(), changes[0]);
}

/// A line of the journal garbled on the card, e.g. with bits flipped, so that it is not UTF-8, or not JSON, or not as
/// its checksum says, is skipped,
/// with the record in it, and the rest of the store still works: reopened, queried, put to, read for changes and
/// compacted, as a capture device's could not be on 23 November 2026, once a line was not UTF-8.
#[test]
fn a_garbled_line_is_skipped_and_the_rest_of_the_store_still_works() {
    let dir = TestDir::new("garbled");
    let hk;
    {
        let store = open(&dir);
        hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
        for (index, value) in [1, 2, 3, 4].into_iter().enumerate() {
            let sk = format!("2026-11-23T04:42:{:02}Z", index * 10);
            store
                .put(&Item::new(
                    &hk,
                    &sk,
                    version(index as i64 + 1, "dev"),
                    json!({"value": value}),
                ))
                .unwrap();
        }
    }
    let journal = std::fs::read_dir(dir.0.join(domain().hex()).join("journal"))
        .unwrap()
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().ends_with(".jsonl"))
        .unwrap()
        .path();
    let text = std::fs::read(&journal).unwrap();
    let mut lines: Vec<Vec<u8>> = text.split(|byte| *byte == b'\n').map(<[u8]>::to_vec).collect();
    // The second record's top bits flipped in its middle, so that it is not UTF-8, and the third's quotes, so that it
    // is not JSON.
    for byte in &mut lines[1][20..40] {
        *byte |= 0x80;
    }
    for byte in lines[2].iter_mut().filter(|byte| **byte == b'"') {
        *byte = b'*';
    }
    // And the fourth's value changed, 4 to 8, a bit flipped, which is still JSON, but not its checksum's.
    let fourth = String::from_utf8(lines[3].clone())
        .unwrap()
        .replace("\"value\":4", "\"value\":8");
    lines[3] = fourth.into_bytes();
    std::fs::write(&journal, lines.join(&b'\n')).unwrap();

    let store = open(&dir);
    let values = |store: &Store| -> Vec<i64> {
        store
            .all(&hk)
            .unwrap()
            .iter()
            .filter_map(|item| item.attributes.as_ref()?.get("value")?.as_i64())
            .collect()
    };
    assert_eq!(values(&store), vec![1]);
    store
        .put(&Item::new(
            &hk,
            "2026-11-23T04:43:00Z",
            version(5, "dev"),
            json!({"value": 5}),
        ))
        .unwrap();
    assert_eq!(values(&store), vec![1, 5]);
    let mut changed = Vec::new();
    store
        .changes(Cursor::START, 100, None, &mut |change| {
            changed.push(change);
            Ok(())
        })
        .unwrap()
        .cursor();
    assert_eq!(changed.len(), 2, "the garbled records are not changes");
    let tomorrow = Micros::now().date().unwrap().next_day().unwrap();
    store.compact(tomorrow).unwrap();
    assert_eq!(values(&store), vec![1, 5]);
}

/// The sequence numbers of the changes after since, up to limit, and the cursor given.
fn sequences(store: &Store, since: u64, limit: usize) -> (Vec<u64>, u64) {
    let mut found = Vec::new();
    let cursor = store
        .changes(Cursor(since), limit, None, &mut |change| {
            found.push(change.sequence);
            Ok(())
        })
        .unwrap()
        .cursor();
    (found, cursor.0)
}

/// The journal is written an hour a file, compacted by the hour, and the hours compacted are kept, from which a peer
/// behind catches up in the order stored; once they have gone, after the retention, the DataSets' files give the same.
#[test]
fn the_journal_is_kept_by_the_hour_and_a_peer_behind_catches_up_from_the_hours_kept() {
    let dir = TestDir::new("hourly");
    let store = open(&dir);
    let hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
    for index in 0..6 {
        let sk = format!("2026-11-23T04:42:{:02}Z", index * 10);
        store
            .put(&Item::new(&hk, &sk, version(index + 1, "dev"), json!({"value": index})))
            .unwrap();
    }
    let domain_dir = dir.0.join(domain().hex());
    let journal: Vec<String> = std::fs::read_dir(domain_dir.join("journal"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".jsonl"))
        .collect();
    assert_eq!(journal.len(), 1, "a file for the hour");
    assert_eq!(
        journal[0].len(),
        "2026-11-23T04.jsonl".len(),
        "named by the hour: {}",
        journal[0]
    );
    assert_eq!(
        std::fs::read_to_string(domain_dir.join("format.json")).unwrap(),
        r#"{"format":2}"#
    );

    // Compacting the hours which have ended, as of an hour from now, folds it, and keeps it.
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    assert!(!domain_dir.join("journal").join(&journal[0]).exists());
    assert!(domain_dir.join("journal/kept").join(&journal[0]).exists());
    assert_eq!(store.all(&hk).unwrap().len(), 6);

    // A peer behind reads the kept hour, a page at a time, in order.
    assert_eq!(sequences(&store, 0, 4), (vec![1, 2, 3, 4], 4));
    assert_eq!(sequences(&store, 4, 4), (vec![5, 6], 6));
    assert_eq!(sequences(&store, 6, 4), (vec![], 6));

    // Puts after it are in a new journal file, and come after the kept ones.
    store
        .put(&Item::new(
            &hk,
            "2026-11-23T05:00:00Z",
            version(7, "dev"),
            json!({"value": 6}),
        ))
        .unwrap();
    assert_eq!(sequences(&store, 4, 4), (vec![5, 6, 7], 7));

    // Reopened, the store has the same, and a new journal file is all that opening reads.
    drop(store);
    let store = open(&dir);
    assert_eq!(store.all(&hk).unwrap().len(), 7);
    assert_eq!(sequences(&store, 2, 10), (vec![3, 4, 5, 6, 7], 7));

    // The hour compacted again, as if put to after the clock was set back, is added to the kept file.
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    assert_eq!(sequences(&store, 4, 4), (vec![5, 6, 7], 7));

    // Once the kept hours have gone, after the retention, a peer behind them is too far behind, and copies instead, see
    // a_peer_too_far_behind_copies_the_store_a_page_at_a_time_then_catches_up; one which has everything is not.
    std::fs::remove_dir_all(domain_dir.join("journal/kept")).unwrap();
    let feed = store.changes(Cursor(4), 4, None, &mut |_| Ok(())).unwrap();
    assert_eq!(feed, Feed::TooFarBehind);
    assert_eq!(sequences(&store, 7, 4), (vec![], 7));
}

/// Every page of a full copy, until it is done, as a peer takes them, with the sort keys given and the cursor to catch
/// up from.
fn copy_all(store: &Store, limit: usize, peer: Option<&str>) -> (Vec<String>, Cursor) {
    let mut found = Vec::new();
    let mut token = None;
    loop {
        let page = store
            .copy(token.as_ref(), limit, peer, &mut |change| {
                found.push(change.item.sk.clone());
                Ok(())
            })
            .unwrap();
        match page {
            CopyPage::More(next) => token = Some(next),
            CopyPage::Done(cursor) => return (found, cursor),
            CopyPage::Expired => panic!("expired"),
        }
    }
}

/// A peer further behind than the kept journal, e.g. a new device, copies the store's DataSets' files a page at a
/// time, every record once, then catches up from the watermark the copy gives, which brings what was put while it
/// copied, even into a file it had passed, and what was still in the journal when it started.
#[test]
fn a_peer_too_far_behind_copies_the_store_a_page_at_a_time_then_catches_up() {
    let dir = TestDir::new("full-copy");
    let store = open(&dir);
    let tesla = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
    let zappi = store.hash_key("myenergi.zappi", &[Part::from("z1")]).unwrap();
    let mut seconds = 0;
    let mut put = |store: &Store, hk: &str, sk: &str| {
        seconds += 1;
        store
            .put(&Item::new(hk, sk, version(seconds, "dev"), json!({"s": seconds})))
            .unwrap();
    };
    for day in 1..=3 {
        for hour in 0..3 {
            put(&store, &tesla, &format!("2026-11-0{day}T0{hour}:00:00Z"));
            put(&store, &zappi, &format!("2026-11-0{day}T0{hour}:30:00Z"));
        }
    }
    put(&store, &zappi, "settings");
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    // Put after compaction, so still in the journal when the copy starts.
    put(&store, &tesla, "2026-11-04T00:00:00Z");
    let domain_dir = dir.0.join(domain().hex());
    std::fs::remove_dir_all(domain_dir.join("journal/kept")).unwrap();
    assert_eq!(
        store.changes(Cursor::START, 10, None, &mut |_| Ok(())).unwrap(),
        Feed::TooFarBehind
    );

    // Pages of 4, the first two pages, then a record changed in a file already passed, and one added.
    let mut found = Vec::new();
    let mut token = None;
    for _ in 0..2 {
        match store
            .copy(token.as_ref(), 4, None, &mut |change| {
                found.push(change.item.sk.clone());
                Ok(())
            })
            .unwrap()
        {
            CopyPage::More(next) => token = Some(next),
            page => panic!("not more: {page:?}"),
        }
    }
    assert_eq!(found.len(), 8);
    let passed = found[0].clone();
    let first_hk = if passed.ends_with(":30:00Z") || passed == "settings" {
        &zappi
    } else {
        &tesla
    };
    put(&store, first_hk, &passed);
    put(&store, &tesla, "2026-11-05T00:00:00Z");
    let watermark = loop {
        match store
            .copy(token.as_ref(), 4, None, &mut |change| {
                found.push(change.item.sk.clone());
                Ok(())
            })
            .unwrap()
        {
            CopyPage::More(next) => token = Some(next),
            CopyPage::Done(cursor) => break cursor,
            CopyPage::Expired => panic!("expired"),
        }
    };
    let mut copied = found.clone();
    copied.sort();
    copied.dedup();
    assert_eq!(copied.len(), found.len(), "each record once");
    assert_eq!(found.len(), 19, "every compacted record, and none of the journal's");
    assert_eq!(watermark, Cursor(19), "the newest compacted");

    // Catching up from the watermark brings the journal's, and those put while copying.
    let mut later = Vec::new();
    let feed = store
        .changes(watermark, 100, None, &mut |change| {
            later.push(change.item.sk.clone());
            Ok(())
        })
        .unwrap();
    assert_eq!(feed, Feed::Next(Cursor(22)));
    assert_eq!(
        later,
        vec![
            "2026-11-04T00:00:00Z".to_string(),
            passed.clone(),
            "2026-11-05T00:00:00Z".to_string()
        ]
    );

    // A copy in pages of 1, or all at once, gives the same records.
    let (one_by_one, cursor) = copy_all(&store, 1, None);
    assert_eq!(cursor, Cursor(19));
    let (at_once, _) = copy_all(&store, 1000, None);
    assert_eq!(one_by_one, at_once);
    assert_eq!(at_once.len(), 19);
}

/// A copy leaves out what came from or was made by the peer asking, as the change feed does, and one which outlasts
/// the kept journal, or whose token is of another store, or damaged, expires, so that the peer starts again.
#[test]
fn a_copy_leaves_out_the_peers_own_and_expires_when_the_journal_kept_has_moved_past_it() {
    let dir = TestDir::new("copy-expires");
    let store = open(&dir);
    let hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
    for index in 0..4 {
        let origin = if index % 2 == 0 { "dev" } else { "peer" };
        store
            .put(&Item::new(
                &hk,
                &format!("2026-11-01T00:00:0{index}Z"),
                version(index + 1, origin),
                json!({"i": index}),
            ))
            .unwrap();
    }
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    let (found, _) = copy_all(&store, 10, Some("peer"));
    assert_eq!(found, vec!["2026-11-01T00:00:00Z", "2026-11-01T00:00:02Z"]);

    let CopyPage::More(token) = store.copy(None, 1, None, &mut |_| Ok(())).unwrap() else {
        panic!("more");
    };
    // Compacted further, and the kept hours gone, so the change feed can no longer reach back to the watermark.
    store
        .put(&Item::new(&hk, "2026-11-02T00:00:00Z", version(9, "dev"), json!({})))
        .unwrap();
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    std::fs::remove_dir_all(dir.0.join(domain().hex()).join("journal/kept")).unwrap();
    assert_eq!(
        store.copy(Some(&token), 1, None, &mut |_| Ok(())).unwrap(),
        CopyPage::Expired
    );
    assert_eq!(
        store
            .copy(Some(&CopyToken("not a token".into())), 1, None, &mut |_| Ok(()))
            .unwrap(),
        CopyPage::Expired
    );
    // Started again, it completes.
    assert_eq!(copy_all(&store, 2, None).0.len(), 5);
}

/// An hour lost from the middle of the kept journal, e.g. by a failing SD card, leaves a gap which a peer behind it
/// would skip without knowing: so the change feed says it is too far behind, and a copy whose catch-up would cross the
/// gap expires, while a peer after the gap catches up as before.
#[test]
fn an_hour_missing_from_the_kept_journal_is_a_gap_which_no_peer_catches_up_across() {
    let dir = TestDir::new("kept-gap");
    let store = open(&dir);
    let hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
    let put = |index: i64| {
        store
            .put(&Item::new(
                &hk,
                &format!("2026-11-01T00:00:{index:02}Z"),
                version(index, "dev"),
                json!({"i": index}),
            ))
            .unwrap();
    };
    for index in 1..=3 {
        put(index);
    }
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    // A copy started now has 3 as its watermark, and catches up across what is compacted after it.
    let CopyPage::More(token) = store.copy(None, 1, None, &mut |_| Ok(())).unwrap() else {
        panic!("more");
    };
    for index in 4..=9 {
        put(index);
    }
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    put(10);

    // The kept hour, holding 1 to 9, split into three hours of three, as if kept over three hours.
    let kept = dir.0.join(domain().hex()).join("journal/kept");
    let names: Vec<_> = std::fs::read_dir(&kept)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(names.len(), 1);
    let text = std::fs::read_to_string(&names[0]).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 9);
    std::fs::remove_file(&names[0]).unwrap();
    for (hour, three) in lines.chunks(3).enumerate() {
        std::fs::write(kept.join(format!("2026-11-20T0{hour}.jsonl")), three.join("\n") + "\n").unwrap();
    }
    assert_eq!(sequences(&store, 0, 100), ((1..=10).collect::<Vec<_>>(), 10));
    assert_eq!(sequences(&store, 2, 3), (vec![3, 4, 5], 5));

    // The middle hour, 4 to 6, lost.
    std::fs::remove_file(kept.join("2026-11-20T01.jsonl")).unwrap();
    for since in [0, 2, 3, 5] {
        let feed = store.changes(Cursor(since), 100, None, &mut |_| Ok(())).unwrap();
        assert_eq!(feed, Feed::TooFarBehind, "from {since}");
    }
    assert_eq!(sequences(&store, 6, 100), (vec![7, 8, 9, 10], 10));
    assert_eq!(
        store.copy(Some(&token), 1, None, &mut |_| Ok(())).unwrap(),
        CopyPage::Expired
    );
    // Started again, its watermark is the newest compacted, after the gap, so it completes, and catches up.
    let (found, cursor) = copy_all(&store, 4, None);
    assert_eq!((found.len(), cursor), (9, Cursor(9)));
    assert_eq!(sequences(&store, cursor.0, 100), (vec![10], 10));
}

/// A store of the layout before, with a journal a file a day, is read and compacted as it is, and marked as of this
/// layout, as what is put now goes in the hour's file.
#[test]
fn a_store_with_a_journal_a_day_is_read_and_compacted_as_it_is() {
    let dir = TestDir::new("daily");
    let hk;
    {
        let store = open(&dir);
        hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
        store
            .put(&Item::new(
                &hk,
                "2026-11-22T10:00:00Z",
                version(1, "dev"),
                json!({"value": 1}),
            ))
            .unwrap();
    }
    let domain_dir = dir.0.join(domain().hex());
    let journal = domain_dir.join("journal");
    let hour = std::fs::read_dir(&journal)
        .unwrap()
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().ends_with(".jsonl"))
        .unwrap()
        .path();
    std::fs::rename(&hour, journal.join("2026-11-22.jsonl")).unwrap();
    std::fs::remove_file(domain_dir.join("format.json")).unwrap();

    let store = open(&dir);
    assert!(domain_dir.join("format.json").exists());
    assert_eq!(store.all(&hk).unwrap().len(), 1);
    store
        .put(&Item::new(
            &hk,
            "2026-11-22T11:00:00Z",
            version(2, "dev"),
            json!({"value": 2}),
        ))
        .unwrap();
    assert_eq!(sequences(&store, 0, 10), (vec![1, 2], 2));
    // Kept for longer than the default, so that the day of 22 November is kept whenever the test runs.
    store.set_retention(std::time::Duration::from_secs(10 * 365 * 24 * 3600));
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    assert!(!journal.join("2026-11-22.jsonl").exists());
    assert!(journal.join("kept/2026-11-22.jsonl").exists());
    assert_eq!(store.all(&hk).unwrap().len(), 2);
    assert_eq!(sequences(&store, 0, 10), (vec![1, 2], 2));
}

/// A day of samples compacted an hour at a time is appended to, not written again, a late record which falls inside
/// it is merged into it, and a query for part of the day, found by bisection, gives the same as reading it all.
#[test]
fn a_series_is_appended_to_and_queried_from_the_middle_of_its_day() {
    let dir = TestDir::new("append");
    let store = open(&dir);
    let hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
    let sk = |index: i64| {
        format!(
            "2026-11-23T{:02}:{:02}:{:02}Z",
            index / 360,
            index / 6 % 60,
            index % 6 * 10
        )
    };
    let put = |index: i64, value: i64| {
        store
            .put(&Item::new(
                &hk,
                &sk(index),
                version(index + value, "dev"),
                json!({"value": value, "pad": "x".repeat(200)}),
            ))
            .unwrap();
    };
    let later = || Micros(Micros::now().0 + 3_600_000_000);
    for index in 0..1000 {
        put(index, 0);
    }
    store.compact_through(later()).unwrap();
    let file = dir.0.join(domain().hex()).join("tesla.snapshot/gw/2026/11/23.jsonl");
    let first_length = std::fs::metadata(&file).unwrap().len();
    for index in 1000..2000 {
        put(index, 0);
    }
    store.compact_through(later()).unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text.lines().count(), 2000);
    assert!(text.len() as u64 > first_length);

    let values = |range: std::ops::Range<i64>| -> Vec<String> {
        store
            .query_all(&hk, sk(range.start).as_str()..sk(range.end).as_str())
            .unwrap()
            .into_iter()
            .map(|item| item.sk)
            .collect()
    };
    let expected = |range: std::ops::Range<i64>| -> Vec<String> { range.map(sk).collect() };
    assert_eq!(values(1500..1520), expected(1500..1520));
    assert_eq!(values(0..3), expected(0..3));
    assert_eq!(values(1995..2000), expected(1995..2000));
    assert_eq!(values(999..1001), expected(999..1001));

    // A late, newer version of a record in the middle is merged into the file, not appended.
    put(500, 1);
    store.compact_through(later()).unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text.lines().count(), 2000);
    let item = store.get(&hk, &sk(500)).unwrap().unwrap();
    assert_eq!(item.attributes.unwrap()["value"], 1);
    assert_eq!(values(499..502), expected(499..502));
    // A put after the file's last line, which needs no index of the file, is found.
    put(2000, 0);
    assert_eq!(values(1999..2001), expected(1999..2001));
}

/// A query's start is found by bisection in files of every size and line length, including lines longer than the
/// bisection's step, which end past its upper bound, which once made it subtract past zero.
#[test]
fn a_query_finds_its_start_in_files_of_long_and_short_lines() {
    let dir = TestDir::new("bisect");
    let store = open(&dir);
    let hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
    let sk = |index: i64| {
        format!(
            "2026-11-23T{:02}:{:02}:{:02}Z",
            index / 3600,
            index / 60 % 60,
            index % 60
        )
    };
    for (index, pad) in [
        (0, 10),
        (1, 9000),
        (2, 10),
        (3, 6000),
        (4, 5000),
        (5, 10),
        (6, 12000),
        (7, 10),
    ] {
        store
            .put(&Item::new(
                &hk,
                &sk(index),
                version(index + 1, "dev"),
                json!({"pad": "x".repeat(pad)}),
            ))
            .unwrap();
    }
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    for from in 0..8 {
        for to in from + 1..=8 {
            let found: Vec<String> = store
                .query_all(&hk, sk(from).as_str()..sk(to).as_str())
                .unwrap()
                .into_iter()
                .map(|item| item.sk)
                .collect();
            assert_eq!(found, (from..to).map(sk).collect::<Vec<_>>(), "from {from} to {to}");
        }
    }
}

/// A copy with compaction rewriting the files under it after every page, and puts to old and new records meanwhile,
/// still leaves the peer, once it has caught up from the watermark, with every record at its newest version.
#[test]
fn a_copy_with_compaction_under_it_and_its_catch_up_give_every_record_as_it_is_now() {
    let dir = TestDir::new("copy-compacting");
    let store = open(&dir);
    let hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
    let mut seconds = 0;
    let mut put = |store: &Store, sk: &str| {
        seconds += 1;
        store
            .put(&Item::new(&hk, sk, version(seconds, "dev"), json!({"s": seconds})))
            .unwrap();
    };
    let later = Micros(Micros::now().0 + 3_600_000_000);
    for day in 1..=4 {
        for minute in 0..5 {
            put(&store, &format!("2026-11-0{day}T00:0{minute}:00Z"));
        }
    }
    store.compact_through(later).unwrap();

    let mut peer: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    let mut take = |change: Change| {
        let s = change.item.attributes.unwrap()["s"].as_i64().unwrap();
        let newest = peer.entry(change.item.sk.clone()).or_insert(s);
        *newest = (*newest).max(s);
        Ok(())
    };
    let mut token = None;
    let mut page = 0;
    let watermark = loop {
        match store.copy(token.as_ref(), 3, None, &mut take).unwrap() {
            CopyPage::More(next) => token = Some(next),
            CopyPage::Done(cursor) => break cursor,
            CopyPage::Expired => panic!("expired"),
        }
        // A record before the copy's place, one after it, and a new day, then compacted, rewriting their files.
        page += 1;
        put(&store, "2026-11-01T00:00:00Z");
        put(&store, &format!("2026-11-04T00:0{}:00Z", page % 5));
        put(&store, &format!("2026-11-{:02}T00:00:00Z", 10 + page));
        store.compact_through(later).unwrap();
    };
    let mut cursor = watermark;
    loop {
        match store.changes(cursor, 4, None, &mut take).unwrap() {
            Feed::Next(next) if next == cursor => break,
            Feed::Next(next) => cursor = next,
            Feed::TooFarBehind => panic!("too far behind"),
        }
    }
    let now: std::collections::BTreeMap<String, i64> = store
        .all(&hk)
        .unwrap()
        .into_iter()
        .map(|item| (item.sk, item.attributes.unwrap()["s"].as_i64().unwrap()))
        .collect();
    assert!(page > 3, "several pages, with compaction between");
    assert_eq!(peer, now);
}

/// A record whose line is damaged on the card, e.g. bits flipped, fails its checksum and is skipped, so the store has
/// it no longer, and the same record sent again by a peer is stored, and replaces the damaged line when compacted.
#[test]
fn a_record_damaged_on_the_card_is_replaced_by_the_same_record_from_a_peer() {
    let dir = TestDir::new("damaged");
    let hk;
    let item;
    let file;
    {
        let store = open(&dir);
        hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
        for index in 1..=3 {
            store
                .put(&Item::new(
                    &hk,
                    &format!("2026-11-01T00:00:0{index}Z"),
                    version(index, "dev"),
                    json!({"value": index}),
                ))
                .unwrap();
        }
        item = store.get(&hk, "2026-11-01T00:00:02Z").unwrap().unwrap();
        store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
        let domain_dir = dir.0.join(domain().hex());
        file = walk(&domain_dir)
            .into_iter()
            .find(|path| {
                path.extension().is_some_and(|extension| extension == "jsonl")
                    && !path.to_string_lossy().contains("journal")
            })
            .unwrap();
    }
    // A digit of the middle record's value changed, as a flipped bit would.
    let text = std::fs::read_to_string(&file).unwrap();
    let damaged = text.replace(r#""value":2"#, r#""value":7"#);
    assert_ne!(text, damaged);
    std::fs::write(&file, damaged).unwrap();

    let store = open(&dir);
    assert_eq!(
        store.get(&hk, "2026-11-01T00:00:02Z").unwrap(),
        None,
        "the damaged line is skipped"
    );
    assert_eq!(store.put_from(&item, "peer").unwrap(), Put::Stored);
    assert_eq!(store.get(&hk, "2026-11-01T00:00:02Z").unwrap(), Some(item.clone()));
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    assert_eq!(store.all(&hk).unwrap().len(), 3);
    assert_eq!(store.get(&hk, "2026-11-01T00:00:02Z").unwrap(), Some(item));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains(r#""value":7"#), "the damaged line is gone");
}

/// A record's line damaged while the store is open, with its index already read, in a compacted file, without its
/// size or time changing, as bits flipped on an SD card would, or in the journal, is taken as missing, not an error,
/// and the same record sent again by a peer replaces it; compaction goes on without a damaged journal line.
#[test]
fn a_record_damaged_while_the_store_is_open_is_missing_and_replaced_by_a_peers_copy() {
    let dir = TestDir::new("damaged-open");
    let store = open(&dir);
    let hk = store.hash_key("tesla.snapshot", &[Part::from("gw")]).unwrap();
    let put = |index: i64| {
        let item = Item::new(
            &hk,
            &format!("2026-11-01T00:00:0{index}Z"),
            version(index, "dev"),
            json!({"value": index}),
        );
        store.put(&item).unwrap();
        item
    };
    let items: Vec<Item> = (1..=3).map(put).collect();
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    assert_eq!(store.get(&hk, &items[1].sk).unwrap(), Some(items[1].clone()));
    let domain_dir = dir.0.join(domain().hex());
    let file = walk(&domain_dir)
        .into_iter()
        .find(|path| {
            path.extension().is_some_and(|extension| extension == "jsonl")
                && !path.to_string_lossy().contains("journal")
        })
        .unwrap();
    let damage = |path: &std::path::Path, from: &str, to: &str| {
        let modified = std::fs::metadata(path).unwrap().modified().unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains(from));
        std::fs::write(path, text.replace(from, to)).unwrap();
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    };

    // In a compacted file.
    damage(&file, r#""value":2"#, r#""value":7"#);
    assert_eq!(store.get(&hk, &items[1].sk).unwrap(), None);
    assert_eq!(store.put_from(&items[1], "peer").unwrap(), Put::Stored);
    assert_eq!(store.get(&hk, &items[1].sk).unwrap(), Some(items[1].clone()));
    // Sent again, now that it is whole, it is a copy already held.
    assert_eq!(store.put_from(&items[1], "peer").unwrap(), Put::Older);

    // In the journal: a record put since, then damaged, which compaction leaves out, and a peer's copy restores.
    let fourth = put(4);
    let journal = walk(&domain_dir.join("journal"))
        .into_iter()
        .find(|path| !path.to_string_lossy().contains("kept") && path.to_string_lossy().ends_with(".jsonl"))
        .unwrap();
    damage(&journal, r#""value":4"#, r#""value":8"#);
    assert_eq!(store.get(&hk, &fourth.sk).unwrap(), None);
    assert_eq!(store.put_from(&fourth, "peer").unwrap(), Put::Stored);
    assert_eq!(store.get(&hk, &fourth.sk).unwrap(), Some(fourth.clone()));
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    assert_eq!(store.all(&hk).unwrap().len(), 4);
    assert_eq!(store.get(&hk, &fourth.sk).unwrap(), Some(fourth));
    assert_eq!(store.get(&hk, &items[1].sk).unwrap(), Some(items[1].clone()));

    // A journal line damaged with no copy sent: compaction goes on without it, and a copy sent later is stored.
    let fifth = put(5);
    let journal = walk(&domain_dir.join("journal"))
        .into_iter()
        .find(|path| !path.to_string_lossy().contains("kept") && path.to_string_lossy().ends_with(".jsonl"))
        .unwrap();
    damage(&journal, r#""value":5"#, r#""value":9"#);
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    assert_eq!(store.all(&hk).unwrap().len(), 4);
    assert_eq!(store.get(&hk, &fifth.sk).unwrap(), None);
    assert_eq!(store.put_from(&fifth, "peer").unwrap(), Put::Stored);
    assert_eq!(store.get(&hk, &fifth.sk).unwrap(), Some(fifth));
}

/// Every file under dir.
fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// A domain's Security Contexts, as the design's example: the domain, whose Admins are the co-owners, an SC for the
/// app under it, and under that `control`, for the admins, and `data`, for the devices and the readers, each with its
/// parent as an Admin member.
struct TestDomain {
    store: Store,
    owner: security::PrincipalSecret,
    wife: security::PrincipalSecret,
    admin: security::PrincipalSecret,
    device: security::PrincipalSecret,
    reader: security::PrincipalSecret,
    domain: Id,
    app: Id,
    control: Id,
    data: Id,
    admin_member: security::MemberRecord,
}

fn put_record(store: &Store, item: anyhow::Result<Item>) {
    store.put(&item.unwrap()).unwrap();
}

/// A moment, so that records made one after another are signed at different times.
fn tick() {
    std::thread::sleep(std::time::Duration::from_millis(2));
}

/// Puts a new principal's records in store, admitted by by, and returns its secret.
fn admit(store: &Store, kind: &str, by: &security::PrincipalSecret) -> security::PrincipalSecret {
    let new = security::NewPrincipal::new(store.domain(), kind).unwrap();
    tick();
    for item in new.record.admitted_by(by).unwrap().items(store, "test").unwrap() {
        store.put(&item).unwrap();
    }
    put_record(store, new.key.item(store, "test"));
    new.secret
}

fn test_domain(dir: &TestDir) -> TestDomain {
    use security::*;
    let founding = Founding::new("Bruce's home").unwrap();
    let domain = Id::parse(&founding.domain.id).unwrap();
    let store = Store::open(&dir.location(), &domain).unwrap();
    let owner = founding.founder.secret.clone();
    for item in founding.founder.record.items(&store, "test").unwrap() {
        store.put(&item).unwrap();
    }
    put_record(&store, founding.founder.key.item(&store, "test"));
    put_record(&store, founding.domain.item(&store, "test"));
    let member = |store: &Store, sc: &Id, member: &Id, role: Role| {
        tick();
        let record = MemberRecord::new(sc, member, role, &owner).unwrap();
        put_record(store, record.item(store, "test"));
        record
    };
    member(&store, &domain, owner.id(), Role::Admin);
    let wife = admit(&store, USER, &owner);
    let admin = admit(&store, USER, &owner);
    let device = admit(&store, SYSTEM, &owner);
    let reader = admit(&store, USER, &owner);
    let (_, app) = ScRecord::new("bruce-smart-home", &owner).unwrap();
    let (_, control) = ScRecord::new("control", &owner).unwrap();
    let (_, data) = ScRecord::new("data", &owner).unwrap();
    let (app_id, control_id, data_id) = (
        Id::parse(&app.id).unwrap(),
        Id::parse(&control.id).unwrap(),
        Id::parse(&data.id).unwrap(),
    );
    for sc in [&app, &control, &data] {
        put_record(&store, sc.item(&store, "test"));
    }
    member(&store, &app_id, &domain, Role::Admin);
    member(&store, &control_id, &app_id, Role::Admin);
    member(&store, &data_id, &app_id, Role::Admin);
    member(&store, &domain, wife.id(), Role::Admin);
    let admin_member = member(&store, &control_id, admin.id(), Role::Write);
    member(&store, &data_id, device.id(), Role::Write);
    member(&store, &data_id, reader.id(), Role::Read);
    TestDomain {
        store,
        owner,
        wife,
        admin,
        device,
        reader,
        domain,
        app: app_id,
        control: control_id,
        data: data_id,
        admin_member,
    }
}

/// Who may do what is worked out from the domain's signed records: each role where it was given, and, through the
/// graph of SCs, the co-owners' Admin everywhere, two levels deep.
#[test]
fn a_domains_security_contexts_give_each_principal_its_role_and_the_co_owners_all() {
    use security::*;
    let dir = TestDir::new("security-roles");
    let domain = test_domain(&dir);
    let store = &domain.store;
    let security = Security::load(store).unwrap();
    for owner in [&domain.owner, &domain.wife] {
        for sc in [&domain.domain, &domain.app, &domain.control, &domain.data] {
            assert_eq!(security.role(owner.id(), sc), Some(Role::Admin));
        }
    }
    assert_eq!(security.role(domain.admin.id(), &domain.control), Some(Role::Write));
    assert_eq!(security.role(domain.admin.id(), &domain.data), None);
    assert_eq!(security.role(domain.admin.id(), &domain.domain), None);
    assert_eq!(security.role(domain.device.id(), &domain.data), Some(Role::Write));
    assert_eq!(security.role(domain.device.id(), &domain.control), None);
    assert_eq!(security.role(domain.reader.id(), &domain.data), Some(Role::Read));
    assert!(security.allows(domain.reader.id(), &domain.data, Role::Read));
    assert!(!security.allows(domain.reader.id(), &domain.data, Role::Write));
    assert_eq!(
        security.sc_path(&["bruce-smart-home", "data"]).map(|sc| sc.id.clone()),
        Some(domain.data.to_string())
    );
    assert_eq!(security.domain().hash.as_deref(), Some(security::HASH));
    assert_eq!(security.principals().count(), 5);

    // A member record signed by someone who is not an Admin of its SC, a device, or an admin of control, does not
    // count.
    let intruder = admit(store, SYSTEM, &domain.owner);
    put_record(
        store,
        MemberRecord::new(&domain.data, intruder.id(), Role::Write, &domain.device).and_then(|m| m.item(store, "t")),
    );
    put_record(
        store,
        MemberRecord::new(&domain.domain, intruder.id(), Role::Admin, &domain.admin).and_then(|m| m.item(store, "t")),
    );
    // Nor does a principal admitted by someone who is not an Admin of the domain, nor one admitting itself.
    let stranger = NewPrincipal::new(store.domain(), USER).unwrap();
    for item in stranger.record.items(store, "test").unwrap() {
        store.put(&item).unwrap();
    }
    put_record(store, stranger.key.item(store, "test"));
    let unadmitted = NewPrincipal::new(store.domain(), USER).unwrap();
    for item in unadmitted
        .record
        .admitted_by(&domain.admin)
        .unwrap()
        .items(store, "test")
        .unwrap()
    {
        store.put(&item).unwrap();
    }
    let security = Security::load(store).unwrap();
    assert_eq!(security.role(intruder.id(), &domain.data), None);
    assert_eq!(security.role(intruder.id(), &domain.domain), None);
    assert!(security.principal(stranger.secret.id()).is_none());
    assert!(security.principal(unadmitted.secret.id()).is_none());
    assert!(security.principal(intruder.id()).is_some());
}

/// A domain's ID is its founder's: another store claiming it, or a domain record changed by someone else, is refused.
#[test]
fn a_domain_is_its_founders() {
    use security::*;
    let dir = TestDir::new("security-domain");
    let domain = test_domain(&dir);
    let store = &domain.store;
    // Another founding's record, put as this domain's, does not have this domain's ID.
    let other = Founding::new("Not Bruce's").unwrap();
    let mut claimed = other.domain.clone();
    claimed.id = domain.domain.to_string();
    put_record(store, claimed.item(store, "test"));
    assert!(Security::load(store).is_err());

    // An Admin may change the domain's record; a writer may not.
    let dir = TestDir::new("security-domain-2");
    let domain = test_domain(&dir);
    let store = &domain.store;
    let mut record = Security::load(store).unwrap().domain().clone();
    record.default = Some(domain.data.to_string());
    record.sign(&domain.wife).unwrap();
    put_record(store, record.item(store, "test"));
    assert_eq!(
        Security::load(store).unwrap().domain().default.as_deref(),
        Some(domain.data.as_str())
    );
    tick();
    record.sign(&domain.admin).unwrap();
    put_record(store, record.item(store, "test"));
    assert!(Security::load(store).is_err());
}

/// Co-owners are equals, so either removes the other, the domain's founder too; what a removed Admin signed before
/// still counts, and what it signs after does not. A removal is not undone by putting the record from before it again,
/// nor by a removed Admin removing the one who removed it, as the records are judged in the order they were signed.
#[test]
fn members_are_removed_by_an_admin_and_a_removal_is_not_undone() {
    use security::*;
    let dir = TestDir::new("security-removal");
    let domain = test_domain(&dir);
    let store = &domain.store;

    // The wife, a co-owner, adds a reader, then removes the owner, who founded the domain.
    let reader2 = admit(store, USER, &domain.wife);
    tick();
    put_record(
        store,
        MemberRecord::new(&domain.data, reader2.id(), Role::Read, &domain.wife).and_then(|m| m.item(store, "t")),
    );
    tick();
    let owner_member = MemberRecord::new(&domain.domain, domain.owner.id(), Role::Admin, &domain.wife).unwrap();
    tick();
    put_record(
        store,
        owner_member.removed_by(&domain.wife).and_then(|m| m.item(store, "t")),
    );
    let security = Security::load(store).unwrap();
    assert_eq!(security.role(domain.owner.id(), &domain.domain), None);
    assert_eq!(security.role(domain.owner.id(), &domain.data), None);
    assert_eq!(security.role(reader2.id(), &domain.data), Some(Role::Read));
    // What the owner signed while it was an Admin still counts.
    assert_eq!(security.role(domain.device.id(), &domain.data), Some(Role::Write));
    assert_eq!(security.role(domain.admin.id(), &domain.control), Some(Role::Write));

    // What the owner signs now does not: a member, nor removing the wife back.
    tick();
    let late = admit(store, USER, &domain.wife);
    tick();
    put_record(
        store,
        MemberRecord::new(&domain.data, late.id(), Role::Read, &domain.owner).and_then(|m| m.item(store, "t")),
    );
    let wife_member = MemberRecord::new(&domain.domain, domain.wife.id(), Role::Admin, &domain.owner).unwrap();
    tick();
    put_record(
        store,
        wife_member.removed_by(&domain.owner).and_then(|m| m.item(store, "t")),
    );
    let security = Security::load(store).unwrap();
    assert_eq!(security.role(late.id(), &domain.data), None);
    assert_eq!(security.role(domain.wife.id(), &domain.domain), Some(Role::Admin));

    // The wife removes the admin; the record from before, put again, as a stolen node could, changes nothing, as it is
    // the same record, at the same time.
    tick();
    put_record(
        store,
        domain
            .admin_member
            .removed_by(&domain.wife)
            .and_then(|m| m.item(store, "t")),
    );
    let security = Security::load(store).unwrap();
    assert_eq!(security.role(domain.admin.id(), &domain.control), None);
    put_record(store, domain.admin_member.item(store, "stolen"));
    let security = Security::load(store).unwrap();
    assert_eq!(security.role(domain.admin.id(), &domain.control), None);
}

/// SCs form a graph: an SC with two parents gives each's members a way in, and a loop of SCs neither stops roles being
/// worked out nor makes two memberships valid by each other.
#[test]
fn the_sc_graph_has_several_parents_and_loops() {
    use security::*;
    let dir = TestDir::new("security-graph");
    let domain = test_domain(&dir);
    let store = &domain.store;
    // A shared SC made by the owner under data, with control as a Read parent too.
    let (_, shared) = ScRecord::new("shared", &domain.owner).unwrap();
    let shared_id = Id::parse(&shared.id).unwrap();
    put_record(store, shared.item(store, "t"));
    tick();
    put_record(
        store,
        MemberRecord::new(&shared_id, &domain.data, Role::Admin, &domain.owner).and_then(|m| m.item(store, "t")),
    );
    tick();
    put_record(
        store,
        MemberRecord::new(&shared_id, &domain.control, Role::Read, &domain.owner).and_then(|m| m.item(store, "t")),
    );
    // A loop: data a Read member of control, as control is, through the app, Admin of data.
    tick();
    put_record(
        store,
        MemberRecord::new(&domain.control, &domain.data, Role::Read, &domain.owner).and_then(|m| m.item(store, "t")),
    );
    let security = Security::load(store).unwrap();
    // The reader reads shared through data; the admin, Write in control, only reads it, through control's Read.
    assert_eq!(security.role(domain.reader.id(), &shared_id), Some(Role::Read));
    assert_eq!(security.role(domain.admin.id(), &shared_id), Some(Role::Read));
    // Round the loop, data's device reads control, and nothing more.
    assert_eq!(security.role(domain.device.id(), &domain.control), Some(Role::Read));
    assert_eq!(security.role(domain.owner.id(), &shared_id), Some(Role::Admin));

    // Two memberships which only an Admin through each other could sign do not count: the device, Write in data, makes
    // a new SC, then makes data its Admin, and claims Admin of the domain through it; it never was one.
    let (_, own) = ScRecord::new("own", &domain.device).unwrap();
    let own_id = Id::parse(&own.id).unwrap();
    put_record(store, own.item(store, "t"));
    tick();
    put_record(
        store,
        MemberRecord::new(&domain.domain, &own_id, Role::Admin, &domain.device).and_then(|m| m.item(store, "t")),
    );
    let security = Security::load(store).unwrap();
    assert_eq!(security.role(domain.device.id(), &domain.domain), None);
    assert_eq!(security.role(domain.device.id(), &own_id), Some(Role::Admin));
}

/// A request signed by a principal is taken as its only for the method, URI, body and time it was signed for, within
/// the window, and only from a principal the domain knows; a principal's record altered after it was signed is
/// ignored; the secrets kept as text sign as before; and names are what the principals were called.
#[test]
fn signed_requests_and_records_are_checked() {
    use security::*;
    let dir = TestDir::new("security-requests");
    let domain = test_domain(&dir);
    let store = &domain.store;
    put_record(
        store,
        DirectoryRecord::name(domain.device.id(), &domain.domain, "Guitron", &domain.owner)
            .and_then(|d| d.item(store, "t")),
    );
    put_record(
        store,
        DirectoryRecord::name(domain.reader.id(), &domain.domain, "Jeff", &domain.reader)
            .and_then(|d| d.item(store, "t")),
    );
    let security = Security::load(store).unwrap();
    assert_eq!(security.display_name(domain.device.id()), Some("Guitron"));
    assert_eq!(security.display_name(domain.reader.id()), Some("Jeff"));
    let now = 1_795_000_000;
    let uri = "/api/store/changes?since=0&limit=500";
    let header = sign_request(&domain.device, "GET", uri, b"", now);
    assert_eq!(
        security
            .check_request(Some(&header), "GET", uri, b"", now + 10)
            .unwrap(),
        Requester {
            subject: domain.device.id().clone(),
            actor: domain.device.id().clone(),
            delegated: false
        }
    );
    assert!(
        security
            .check_request(Some(&header), "GET", "/api/config", b"", now)
            .is_err()
    );
    assert!(security.check_request(Some(&header), "POST", uri, b"{}", now).is_err());
    assert!(
        security
            .check_request(Some(&header), "GET", uri, b"", now + REQUEST_WINDOW + 1)
            .is_err()
    );
    assert!(security.check_request(None, "GET", uri, b"", now).is_err());
    let stranger = NewPrincipal::new(store.domain(), SYSTEM).unwrap();
    let header = sign_request(&stranger.secret, "GET", uri, b"", now);
    assert!(security.check_request(Some(&header), "GET", uri, b"", now).is_err());

    // Kept as text and read back, a secret signs as the principal it is.
    let kept = PrincipalSecret::from_text(&domain.reader.to_text()).unwrap();
    let header = sign_request(&kept, "GET", uri, b"", now);
    assert_eq!(
        security
            .check_request(Some(&header), "GET", uri, b"", now)
            .unwrap()
            .subject,
        domain.reader.id().clone()
    );
    assert!(!format!("{kept:?}").contains(&kept.to_text()));
    let (sc_secret, _) = ScRecord::new("other", &domain.owner).unwrap();
    assert_eq!(
        ScSecret::from_text(&sc_secret.to_text()).unwrap().to_text(),
        sc_secret.to_text()
    );

    // A principal's record given another kind without being signed again does not count.
    let hk = PrincipalRecord::hash_key(store, domain.reader.id()).unwrap();
    let mut record: PrincipalRecord =
        serde_json::from_value(store.get(&hk, RECORD_SK).unwrap().unwrap().attributes.unwrap()).unwrap();
    record.kind = SYSTEM.to_string();
    tick();
    record.at = Micros::now().to_text();
    for item in record.items(store, "test").unwrap() {
        store.put(&item).unwrap();
    }
    let security = Security::load(store).unwrap();
    assert!(security.principal(domain.reader.id()).is_none());
    assert_eq!(security.role(domain.reader.id(), &domain.data), None);
}

/// A user present signs with their app as actor, and the user's authority is used; while they are away, their app acts
/// for them by a delegation, up to its role and theirs, never Admin; ending it stops the app, and the record from
/// before, put again, does not undo it, as it is older.
#[test]
fn an_actor_acts_for_its_subject_present_or_by_delegation() {
    use security::*;
    let dir = TestDir::new("security-actors");
    let domain = test_domain(&dir);
    let store = &domain.store;
    let app = admit(store, SYSTEM, &domain.owner);
    let now = Micros::now().0 / 1_000_000;
    let uri = "/api/config";

    // Present: the owner signs, and the app, which has no role of its own, signs as actor.
    let security = Security::load(store).unwrap();
    let header = sign_request_for(Subject::Present(&domain.owner), &app, "POST", uri, b"{}", now);
    let requester = security.check_request(Some(&header), "POST", uri, b"{}", now).unwrap();
    assert_eq!(requester.subject, *domain.owner.id());
    assert_eq!(requester.actor, *app.id());
    assert!(!requester.delegated);
    assert_eq!(security.role_for(&requester, &domain.domain), Some(Role::Admin));
    assert!(security.sees_security_records(&requester));
    // The subject and actor are signed, so neither can be swapped for another.
    let swapped = header.replace(app.id().as_str(), domain.admin.id().as_str());
    assert!(security.check_request(Some(&swapped), "POST", uri, b"{}", now).is_err());

    // Away, without a delegation: refused.
    let away = sign_request_for(Subject::Away(domain.owner.id()), &app, "GET", uri, b"", now);
    assert!(security.check_request(Some(&away), "GET", uri, b"", now).is_err());

    // With one, up to Write: the owner's Admin is capped at Write, everywhere.
    assert!(DelegationRecord::new(&domain.owner, app.id(), Role::Admin).is_err());
    let delegation = DelegationRecord::new(&domain.owner, app.id(), Role::Write).unwrap();
    put_record(store, delegation.item(store, "test"));
    let security = Security::load(store).unwrap();
    let requester = security.check_request(Some(&away), "GET", uri, b"", now).unwrap();
    assert!(requester.delegated);
    for sc in [&domain.domain, &domain.control, &domain.data] {
        assert_eq!(security.role_for(&requester, sc), Some(Role::Write));
    }
    assert!(security.sees_security_records(&requester));
    // A reader's app delegated Write may still only read, and is not given the security records.
    let jeffs_app = admit(store, SYSTEM, &domain.owner);
    put_record(
        store,
        DelegationRecord::new(&domain.reader, jeffs_app.id(), Role::Write)
            .unwrap()
            .item(store, "test"),
    );
    // A delegation signed by someone other than its subject does not count.
    let mut forged = DelegationRecord::new(&domain.admin, jeffs_app.id(), Role::Write).unwrap();
    forged.subject = domain.wife.id().to_string();
    put_record(store, forged.item(store, "test"));
    let security = Security::load(store).unwrap();
    let jeff = security
        .check_request(
            Some(&sign_request_for(
                Subject::Away(domain.reader.id()),
                &jeffs_app,
                "GET",
                uri,
                b"",
                now,
            )),
            "GET",
            uri,
            b"",
            now,
        )
        .unwrap();
    assert_eq!(security.role_for(&jeff, &domain.data), Some(Role::Read));
    assert_eq!(security.role_for(&jeff, &domain.control), None);
    assert!(!security.sees_security_records(&jeff));
    assert!(security.delegation(domain.wife.id(), jeffs_app.id()).is_none());

    // Ended, and not undone by the older record put again, as the newer version wins.
    tick();
    let ended = delegation.ended_by(&domain.owner).unwrap();
    put_record(store, ended.item(store, "test"));
    let security = Security::load(store).unwrap();
    assert!(security.check_request(Some(&away), "GET", uri, b"", now).is_err());
    put_record(store, delegation.item(store, "stolen"));
    let security = Security::load(store).unwrap();
    assert!(security.check_request(Some(&away), "GET", uri, b"", now).is_err());
}

/// A principal's first key is the one its ID was made from, and commits to its next; a key stops being valid at its
/// until; and the principal's DataSets are split by their IDs' first two hex characters on disk.
#[test]
fn keys_commit_to_their_next_and_principals_are_split_on_disk() {
    use security::*;
    let dir = TestDir::new("security-keys");
    let domain = test_domain(&dir);
    let store = &domain.store;
    let new = NewPrincipal::new(store.domain(), USER).unwrap();
    assert!(new.record.verify_own().is_ok());
    assert_eq!(new.key.sign, new.record.first);
    assert_eq!(new.key.next, new.record.next);
    assert!(new.key.valid_at(Micros::now()));
    let mut ended = new.key.clone();
    let until = Micros::now();
    ended.until = Some(until.to_text());
    assert!(ended.valid_at(Micros(until.0 - 1)));
    assert!(!ended.valid_at(until));
    // A record claiming another first key is not its ID's.
    let mut other = new.record.clone();
    other.first = NewPrincipal::new(store.domain(), USER).unwrap().record.first;
    assert!(other.check_id().is_err());

    store.sync().unwrap();
    store.compact_through(Micros(Micros::now().0 + 3_600_000_000)).unwrap();
    let hex = domain.reader.id().hex();
    let split = walk(&dir.0).into_iter().any(|path| {
        path.to_string_lossy()
            .contains(&format!("principal/{}/{hex}", &hex[..2]))
    });
    assert!(
        split,
        "the principal's DataSet is not split by its ID's first hex characters"
    );
    assert!(is_security_data_set(&PrincipalRecord::list_hash_key(store).unwrap()));
    assert!(!is_security_data_set(
        &store.hash_key("sensor.temperature", &[]).unwrap()
    ));
}
