//! Home's path over ssh: quoted for openrsync, left for rsync 3 to escape.
//!
//! Two layers. The word replicant *builds* is checked per [`Client`] with the
//! detection mocked. What the real local rsync then hands a fake ssh depends
//! on which rsync this machine has (openrsync passes it raw, rsync 3 escapes
//! it itself), so that layer is checked with the detected client and judged
//! by the property that matters: home's shell must get the intended path back
//! and run nothing.

use super::*;
use crate::testutil;

const NASTY: &str = "/home/b/work/it's a (repo) $(touch PWNED)";

#[test]
fn version_banners_classify() {
    let open = "openrsync: protocol version 29\nrsync version 2.6.9 compatible\n";
    let samba = "rsync  version 3.2.7  protocol version 31\nCopyright (C) 1996-2022 by Andrew Tridgell, Wayne Davison, and others.\n";
    let old = "rsync  version 2.6.9  protocol version 29\n";
    assert_eq!(Client::from_version(open), Client::OpenRsync);
    assert_eq!(Client::from_version(samba), Client::Rsync);
    assert_eq!(Client::from_version(old), Client::Rsync);
}

#[test]
fn detect_runs_and_is_stable() {
    let a = Client::detect().unwrap();
    assert_eq!(Client::detect().unwrap(), a);
}

#[test]
fn quoting_round_trips_through_a_shell_without_running_anything() {
    let tmp = tempfile::tempdir().unwrap();
    let word = client::shell_quote(NASTY);
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("printf '%s' {word}"))
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), NASTY);
    assert!(!tmp.path().join("PWNED").exists());
    assert_eq!(client::shell_quote("plain"), "'plain'");
    assert_eq!(client::shell_quote(""), "''");
}

fn nasty_pull(tmp: &Path) -> Transfer {
    Transfer {
        direction: Direction::Pull,
        leaf_path: tmp.join("leaf"),
        home_path: NASTY.into(),
        home_host: Some("thistledown".into()),
        is_file: false,
        excludes: Excludes::for_root(&[]).unwrap(),
        source_absent: false,
    }
}

/// The remote word replicant builds for a client, before any rsync sees it.
fn built_remote_word(client: Client) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let r = Runner::new(Duration::from_secs(1), client, "/nonexistent".into());
    let a = nasty_pull(tmp.path()).args(&r, Path::new("/e"));
    a[a.len() - 2].clone()
}

#[test]
fn openrsync_single_quotes_home_path() {
    assert_eq!(
        built_remote_word(Client::OpenRsync),
        r"thistledown:'/home/b/work/it'\''s a (repo) $(touch PWNED)/'"
    );
}

#[test]
fn rsync3_leaves_home_path_for_its_own_escaping() {
    assert_eq!(
        built_remote_word(Client::Rsync),
        "thistledown:/home/b/work/it's a (repo) $(touch PWNED)/"
    );
}

/// With this machine's real rsync and its detected client: the last word the
/// fake ssh receives, evaluated the way home's shell would evaluate it, is
/// the intended path and runs nothing.
#[test]
fn home_shell_gets_the_intended_path_and_runs_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let argv = tmp.path().join("argv");
    let ssh = testutil::script(
        tmp.path(),
        "ssh",
        &format!("printf '%s\\n' \"$@\" > {}\nexit 1", argv.display()),
    );
    let runner = Runner {
        ssh: ssh.display().to_string(),
        ..Runner::new(
            Duration::from_secs(30),
            Client::detect().unwrap(),
            "/nonexistent".into(),
        )
    };
    let x = nasty_pull(tmp.path());
    assert!(x.dry_run(&runner).is_err(), "the fake ssh is not a server");
    let got = std::fs::read_to_string(&argv).unwrap();
    assert!(got.lines().any(|l| l == "thistledown"), "{got}");
    let word = got.lines().last().unwrap().to_string();
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("printf '%s' {word}"))
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("{NASTY}/"),
        "ssh word was: {word}"
    );
    assert!(!tmp.path().join("PWNED").exists());
}

#[test]
fn local_paths_are_never_quoted() {
    let x = Transfer {
        direction: Direction::Push,
        leaf_path: NASTY.into(),
        home_path: "/h/w".into(),
        home_host: None,
        is_file: false,
        excludes: Excludes::for_root(&[]).unwrap(),
        source_absent: false,
    };
    let r = Runner::new(
        Duration::from_secs(1),
        Client::OpenRsync,
        "/nonexistent".into(),
    );
    let a = x.args(&r, Path::new("/e"));
    assert_eq!(&a[a.len() - 2..], [format!("{NASTY}/"), "/h/w/".into()]);
}
