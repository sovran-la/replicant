//! What the person sees while the daemon applies: each unit's line as it
//! starts and ends, in order, then the end line with the total; with
//! `--json`, one object per line; without a terminal, the stream on stderr
//! and stdout's report as before.

use super::*;

/// `pull_bed` synced at the prompt: two units, both pulled.
fn at_the_prompt(verbose: bool) -> Run {
    let (bed, home) = pull_bed();
    let a = Args {
        verbose,
        ..tty(&bed)
    };
    let r = cli(&bed, &home, &a, vec![answer("y")]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    r
}

/// The lines after the question, each unit's start and end on one line.
fn unit_lines(out: &str) -> Vec<&str> {
    let after = out.split_once("Apply this plan? [y/N] ").expect(out).1;
    after.lines().filter(|l| l.starts_with("  [")).collect()
}

#[test]
fn each_unit_is_shown_as_it_runs_then_the_total() {
    let r = at_the_prompt(false);
    let lines = unit_lines(&r.out);
    assert_eq!(lines.len(), 2, "{}", r.out);
    assert!(
        lines[0].starts_with("  [1/2] pull  work … 1 file  "),
        "{}",
        r.out
    );
    assert!(lines[1].starts_with("  [2/2] pull  work/r … "), "{}", r.out);
    for l in &lines {
        assert!(l.ends_with('s'), "ends with its time: {l}");
    }
    let done = r.out.lines().last().unwrap();
    assert!(done.starts_with("done: 2 units applied in "), "{}", r.out);
    assert!(done.ends_with('s'), "{done}");
    assert!(!r.out.contains("applied:\n"), "not listed twice: {}", r.out);
    assert!(r.err.is_empty(), "{}", r.err);
}

#[test]
fn verbose_lists_the_units_again_with_their_times() {
    let r = at_the_prompt(true);
    assert_eq!(unit_lines(&r.out).len(), 2, "{}", r.out);
    let list = r.out.split_once("applied:\n").expect(&r.out).1;
    let rows: Vec<&str> = list.lines().take_while(|l| l.starts_with("  ")).collect();
    assert_eq!(rows.len(), 2, "{list}");
    assert!(
        rows[1].contains("work/r") && rows[1].ends_with('s'),
        "{}",
        rows[1]
    );
}

#[test]
fn yes_at_a_terminal_shows_the_units_not_the_plan_again() {
    let (bed, home) = pull_bed();
    let a = Args {
        yes: true,
        ..tty(&bed)
    };
    let r = cli(&bed, &home, &a, vec![]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.starts_with("  [1/2] pull  work … "), "{}", r.out);
    assert!(!r.out.contains("pull ← home"), "{}", r.out);
}

#[test]
fn json_is_one_object_per_line_events_then_the_report() {
    let (bed, home) = pull_bed();
    let a = Args {
        yes: true,
        json: true,
        ..tty(&bed)
    };
    let r = cli(&bed, &home, &a, vec![]);
    assert_eq!(r.code, 0, "{}", r.err);
    let objs: Vec<serde_json::Value> = r
        .out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect();
    assert_eq!(objs.len(), 5, "{}", r.out);
    let kinds: Vec<&str> = objs[..4]
        .iter()
        .map(|o| o["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["unit_started", "unit_done", "unit_started", "unit_done"]
    );
    assert_eq!(objs[0]["rel"], "work");
    assert_eq!(objs[0]["direction"], "pull");
    assert_eq!(
        (objs[2]["index"].as_u64(), objs[2]["total"].as_u64()),
        (Some(2), Some(2))
    );
    assert_eq!(objs[3]["outcome"]["kind"], "applied");
    assert!(objs[3]["elapsed_ms"].is_u64());
    let report = &objs[4];
    assert_eq!(report["outcome"]["outcome"], "done");
    assert_eq!(report["applied"].as_array().unwrap().len(), 2);
    assert!(report["elapsed_ms"].is_u64());
    assert!(r.err.is_empty(), "{}", r.err);
}

#[test]
fn without_a_terminal_the_stream_goes_to_stderr_and_stdout_keeps_the_report() {
    let (bed, home) = pull_bed();
    let a = Args {
        yes: true,
        interactive: false,
        ..tty(&bed)
    };
    let r = cli(&bed, &home, &a, vec![]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(
        unit_lines(&format!("Apply this plan? [y/N] {}", r.err)).len(),
        2,
        "{}",
        r.err
    );
    assert!(!r.out.contains("  [1/2]"), "{}", r.out);
    assert!(
        r.out.contains("pull ← home"),
        "the plan, as before: {}",
        r.out
    );
    assert!(
        r.out.contains("applied:\n"),
        "the units, as before: {}",
        r.out
    );
    assert!(r.out.contains("done: 2 units applied in "), "{}", r.out);

    // --json without a terminal: stdout is the report alone, one line.
    let (bed, home) = pull_bed();
    let j = Args {
        json: true,
        home_dir: bed.leaf_hd.clone(),
        ..a
    };
    let r = cli(&bed, &home, &j, vec![]);
    assert_eq!(r.out.lines().count(), 1, "{}", r.out);
    let report: serde_json::Value = serde_json::from_str(r.out.trim()).unwrap();
    assert_eq!(report["outcome"]["outcome"], "done");
    assert_eq!(r.err.lines().count(), 4, "{}", r.err);
}
