//! Throughput of the native transfer through the real daemons over mTLS on
//! loopback: a pull of 10,000 small files, and of one 2 GiB file, with 1
//! and 4 streams. Ignored by default (it writes 4 GiB); run it with
//! `cargo test --release -- --ignored --nocapture bench`.

use super::test_certs::pair;
use super::*;
use crate::plan::apply::{Outcome, SyncOpts};
use crate::testutil::{self, Bed};
use std::io::Write;
use std::time::Instant;

fn small(root: &std::path::Path) -> u64 {
    for i in 0..10_000 {
        testutil::write(
            &root.join(format!("d{:02}/f{i:05}.txt", i % 100)),
            &format!("{i:0>1000}\n"),
        );
    }
    10_000 * 1001
}

fn big(root: &std::path::Path) -> u64 {
    std::fs::create_dir_all(root).unwrap();
    let mut f = std::fs::File::create(root.join("blob.bin")).unwrap();
    let chunk: Vec<u8> = (0..(1u32 << 20))
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    for _ in 0..2048 {
        f.write_all(&chunk).unwrap();
    }
    f.sync_all().unwrap();
    2048 << 20
}

fn run(name: &str, fill: fn(&std::path::Path) -> u64, streams: u32) {
    let bed = Bed::enrolled();
    testutil::hand_edit(
        &bed.home_dirs.fleet(),
        "listen = ",
        &format!("streams = {streams}\nlisten = "),
    );
    let bytes = fill(&bed.home_hd.join("work"));
    let (_home, _leaf) = pair(&bed);
    let started = Instant::now();
    let LocalReply::Synced(r) = local::call(
        &bed.leaf_dirs,
        LocalRequest::Sync {
            opts: SyncOpts::default(),
        },
    )
    .unwrap() else {
        panic!()
    };
    let total = started.elapsed();
    assert_eq!(r.outcome, Outcome::Done, "{r:#?}");
    let unit = r
        .applied
        .iter()
        .find(|a| a.rel == "work")
        .expect("the unit");
    let secs = unit.elapsed_ms as f64 / 1000.0;
    let ex = crate::exclude::Excludes::for_root(&["target".into()]).unwrap();
    let (a, b) = (
        crate::native::Manifest::walk(&bed.home_hd.join("work"), &ex, false).unwrap(),
        crate::native::Manifest::walk(&bed.leaf_hd.join("work"), &ex, false).unwrap(),
    );
    assert_eq!(a, b, "the leaf holds home's tree");
    eprintln!(
        "BENCH {name:>9} streams={streams}: unit {secs:6.2}s  {:7.1} MB/s  {:7.0} files/s  (whole sync {:.2}s)",
        bytes as f64 / 1e6 / secs,
        unit.files_changed as f64 / secs,
        total.as_secs_f64(),
    );
}

#[test]
#[ignore = "writes 4 GiB; run on purpose with --release --ignored"]
fn bench_pull_over_mtls_loopback() {
    for streams in [1, 4] {
        run("10k small", small, streams);
        run("one 2 GiB", big, streams);
    }
}
