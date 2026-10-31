use super::*;

fn line(ts: u64, event: &str, host: &str, path: &str) -> Line {
    Line {
        ts,
        event: event.into(),
        host: host.into(),
        path: path.into(),
        detail: "d e t".into(),
    }
}

#[test]
fn render_parse_round_trip() {
    let l = line(1_793_473_451, "dry-run", "birchbook", "work/tundra");
    let text = l.render();
    assert_eq!(
        text,
        "2026-10-31T19:04:11Z\tdry-run\tbirchbook\twork/tundra\td e t"
    );
    assert_eq!(Line::parse(&text), Some(l));
}

#[test]
fn tabs_and_newlines_in_fields_cannot_shift_columns() {
    let mut l = line(5, "x", "h", "p");
    l.detail = "a\tb\nc".into();
    let parsed = Line::parse(&l.render()).unwrap();
    assert_eq!(parsed.detail, "a b c");
}

#[test]
fn empty_path_is_dash() {
    assert_eq!(Line::new("approve", "spruce", "", "allow:(none)").path, "-");
}

#[test]
fn journal_round_trip_with_filters() {
    let tmp = tempfile::tempdir().unwrap();
    let j = Journal::at(tmp.path().join("j/journal.log"));
    assert!(
        j.read(&Filter::default()).unwrap().is_empty(),
        "missing file reads empty"
    );
    j.append(&line(100, "subscribe", "birchbook", "work"))
        .unwrap();
    j.append(&line(200, "dry-run", "twinoaks", "work/tundra"))
        .unwrap();
    j.append(&line(300, "approve", "spruce", "-")).unwrap();
    assert_eq!(j.read(&Filter::default()).unwrap().len(), 3);
    let by_host = Filter {
        host: Some("twinoaks".into()),
        ..Default::default()
    };
    assert_eq!(j.read(&by_host).unwrap()[0].event, "dry-run");
    let by_path = Filter {
        path: Some("work/tundra/beacon".into()),
        ..Default::default()
    };
    let hits: Vec<_> = j
        .read(&by_path)
        .unwrap()
        .into_iter()
        .map(|l| l.ts)
        .collect();
    assert_eq!(hits, vec![100, 200], "events above and at the path");
    let since = Filter {
        since: Some(200),
        ..Default::default()
    };
    assert_eq!(j.read(&since).unwrap().len(), 2);
}

#[test]
fn append_raw_refuses_malformed_and_read_skips_torn_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("journal.log");
    let j = Journal::at(p.clone());
    assert!(j.append_raw("not a journal line").is_err());
    j.append(&line(1, "a", "h", "p")).unwrap();
    let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
    f.write_all(b"2026-10-31T19:0").unwrap();
    drop(f);
    j.append(&line(2, "b", "h", "p")).unwrap();
    let events: Vec<_> = j
        .read(&Filter::default())
        .unwrap()
        .into_iter()
        .map(|l| l.event)
        .collect();
    assert_eq!(
        events,
        vec!["a"],
        "the torn line swallowed the next append's line start"
    );
}

#[test]
fn concurrent_appends_do_not_interleave() {
    let tmp = tempfile::tempdir().unwrap();
    let j = std::sync::Arc::new(Journal::at(tmp.path().join("journal.log")));
    let handles: Vec<_> = (0..8)
        .map(|t| {
            let j = j.clone();
            std::thread::spawn(move || {
                for i in 0..50 {
                    j.append(&line(t * 1000 + i, "ev", &format!("h{t}"), "work"))
                        .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(j.read(&Filter::default()).unwrap().len(), 400);
}
