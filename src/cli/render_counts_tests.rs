use super::*;

#[test]
fn counts_read_as_people_write_them() {
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(70_713), "70,713");
    assert_eq!(thousands(1_234_567), "1,234,567");
    assert_eq!(bytes(0), "0 B");
    assert_eq!(bytes(999), "999 B");
    assert_eq!(bytes(1000), "1.0 kB");
    assert_eq!(bytes(3_200_000_000), "3.2 GB");
    assert_eq!(bytes(14_100_000_000), "14.1 GB");
    assert_eq!(bytes(999_960), "1.0 MB", "no 1000.0 kB");
    assert_eq!(
        counters(12_345, 70_713, 3_200_000_000, 14_100_000_000),
        "12,345/70,713 files  3.2 GB/14.1 GB"
    );
}

#[test]
fn a_progress_event_redraws_the_units_line() {
    let e = ProgressEvent::UnitProgress {
        rel: "work".into(),
        action: "push".into(),
        index: 1,
        total: 155,
        files_done: 12_345,
        files_total: 70_713,
        bytes_done: 3_200_000_000,
        bytes_total: 14_100_000_000,
    };
    assert_eq!(
        redraw(&e),
        "\r\x1b[2K  [1/155] push  work … 12,345/70,713 files  3.2 GB/14.1 GB"
    );
    let started = ProgressEvent::UnitStarted {
        rel: "work".into(),
        action: "push".into(),
        direction: None,
        index: 1,
        total: 2,
    };
    assert_eq!(redraw(&started), "");
}

#[test]
fn counters_redraw_the_running_line_at_a_terminal_and_nowhere_else() {
    use crate::plan::apply::UnitOutcome;
    let st = crate::cli::render::Style { color: false };
    let start = ProgressEvent::UnitStarted {
        rel: "work".into(),
        action: "push".into(),
        direction: None,
        index: 1,
        total: 1,
    };
    let tick = |n| ProgressEvent::UnitProgress {
        rel: "work".into(),
        action: "push".into(),
        index: 1,
        total: 1,
        files_done: n,
        files_total: 2,
        bytes_done: n * 10,
        bytes_total: 20,
    };
    let done = ProgressEvent::UnitDone {
        rel: "work".into(),
        action: "push".into(),
        direction: None,
        files_changed: 2,
        deleted: 0,
        elapsed_ms: 1400,
        push: None,
        outcome: UnitOutcome::Applied,
    };
    let feed = |l: &mut Lines| -> String {
        [&start, &tick(1), &tick(2), &done]
            .iter()
            .map(|e| l.line(e))
            .collect()
    };
    let term = feed(&mut Lines::new(false, true, st));
    assert_eq!(
        term,
        "  [1/1] push  work … \r\x1b[2K  [1/1] push  work … 1/2 files  10 B/20 B\
         \r\x1b[2K  [1/1] push  work … 2/2 files  20 B/20 B\
         \r\x1b[2K  [1/1] push  work … 2 files  1.4s\n"
    );
    // What the terminal shows at the end: the start, then the outcome.
    assert_eq!(
        term.rsplit("\x1b[2K").next(),
        Some("  [1/1] push  work … 2 files  1.4s\n")
    );

    let log = feed(&mut Lines::new(false, false, st));
    assert_eq!(
        log, "  [1/1] push  work … 2 files  1.4s\n",
        "no counters in a log"
    );

    let json = feed(&mut Lines::new(true, false, st));
    assert_eq!(json.lines().count(), 4);
    assert!(
        json.lines()
            .nth(1)
            .unwrap()
            .contains("\"event\":\"unit_progress\""),
        "{json}"
    );
}
