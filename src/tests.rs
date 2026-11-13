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

fn site() -> Id {
    Id::of(&json!({"site": "test"})).unwrap()
}

fn version(seconds: i64, origin: &str) -> Version {
    Version::new(Micros(seconds * 1_000_000), origin)
}

fn open(dir: &TestDir) -> Store {
    Store::open(&dir.location(), &site()).unwrap()
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
        std::fs::read_dir(dir.0.join(site().hex()).join("journal"))
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".jsonl"))
    );
    assert_eq!(values(&store, "2026-11-04".."2026-11-05"), vec![2, 3]);
    assert!(
        dir.0
            .join(site().hex())
            .join("tesla.snapshot/gw/2026/11/04.jsonl")
            .exists()
    );
    assert!(
        dir.0
            .join(site().hex())
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
            .unwrap();
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
        store.data_sets(&format!("{}/octopus.", site())).unwrap(),
        vec![readings.clone()]
    );
    store
        .compact(Micros::now().date().unwrap().next_day().unwrap())
        .unwrap();
    assert_eq!(
        store.data_sets(&format!("{}/", site())).unwrap(),
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
    let other = Id::of(&json!({"site": "other"})).unwrap();
    let hk = hash_key(&other, "climate.period", &[]).unwrap();
    assert!(store.put(&Item::new(&hk, "-", version(1, "app"), json!({}))).is_err());
    assert!(
        store
            .put(&Item::new(
                &format!("{}/Bad Kind", site()),
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
    let dir = TestDir::new("copy-site");
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

    let copy = Id::of(&json!({"site": "copy"})).unwrap();
    Store::copy_site(&dir.location(), &site(), &copy).unwrap();
    let copied = Store::open(&dir.location(), &copy).unwrap();
    let copied_hk = copied.hash_key("climate.period", &[Part::from("dev1")]).unwrap();
    assert_eq!(
        copied.all(&copied_hk).unwrap().len(),
        2,
        "from the compacted files and the journal"
    );
    assert_eq!(copied.get_blob(&blob).unwrap().as_deref(), Some(&b"a bill"[..]));
    assert!(
        Store::copy_site(&dir.location(), &site(), &copy).is_err(),
        "not over a site which is there"
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
            .unwrap();
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
    let copy = Id::of(&json!({"site": "copy"})).unwrap();
    Store::copy_site(&dir.location(), &site(), &copy).unwrap();
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
        .unwrap();
    let json = changes[0].to_json().unwrap();
    assert_eq!(json["hk"], json!(hk));
    assert_eq!(json["from"], json!("dev1"));
    assert_eq!(Change::from_json(json).unwrap(), changes[0]);
}
