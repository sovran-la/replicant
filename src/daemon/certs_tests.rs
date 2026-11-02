use super::*;
use crate::daemon::tls::Identity;

fn paths(dir: &Path) -> Paths {
    Paths::of(&crate::config::Dirs::at(dir.to_path_buf()))
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

#[test]
fn ensure_generates_once_and_never_rotates() {
    let tmp = tempfile::tempdir().unwrap();
    let p = paths(tmp.path());
    assert!(ensure(&p).unwrap());
    let (server, boot) = (read(&p.server), read(&p.bootstrap));
    for b in [&server, &boot] {
        assert_eq!(b.matches("-----BEGIN CERTIFICATE-----").count(), 2);
        assert_eq!(b.matches("PRIVATE KEY-----").count(), 2, "BEGIN and END");
    }
    assert_ne!(server, boot, "home's server identity is not the bootstrap");
    assert!(!ensure(&p).unwrap());
    assert_eq!((read(&p.server), read(&p.bootstrap)), (server, boot));
    for f in [&p.ca_key, &p.ca_cert, &p.server, &p.bootstrap] {
        let mode = std::fs::metadata(f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, FILE_MODE, "{}", f.display());
    }
}

#[test]
fn a_deleted_bundle_is_reissued_from_the_same_ca() {
    let tmp = tempfile::tempdir().unwrap();
    let p = paths(tmp.path());
    ensure(&p).unwrap();
    let ca = read(&p.ca_cert);
    for gone in [&p.bootstrap, &p.server] {
        let before = read(gone);
        std::fs::remove_file(gone).unwrap();
        assert!(ensure(&p).unwrap());
        let after = read(gone);
        assert!(after.contains(ca.trim()), "{}", gone.display());
        assert_ne!(after, before, "a new key");
    }
}

/// The deployed fleet's upgrade: a v0.1 home has a CA and one shared bundle
/// and no server.pem. The bundle is kept (it becomes the bootstrap) and
/// server.pem is minted beside it from the same CA.
#[test]
fn a_v0_1_home_gains_a_server_identity_and_keeps_its_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let p = paths(tmp.path());
    ensure(&p).unwrap();
    let legacy = legacy_bundle(&p).unwrap();
    std::fs::write(&p.bootstrap, &legacy).unwrap();
    std::fs::remove_file(&p.server).unwrap();
    assert!(ensure(&p).unwrap());
    assert_eq!(read(&p.bootstrap), legacy, "the shared bundle is untouched");
    assert!(read(&p.server).contains(read(&p.ca_cert).trim()));
}

#[test]
fn a_missing_ca_beside_a_bundle_is_an_error_not_a_new_ca() {
    let tmp = tempfile::tempdir().unwrap();
    let p = paths(tmp.path());
    ensure(&p).unwrap();
    std::fs::remove_file(&p.ca_key).unwrap();
    std::fs::remove_file(&p.server).unwrap();
    let err = ensure(&p).unwrap_err().to_string();
    assert!(err.contains("CA") && err.contains("missing"), "{err}");
    assert!(!p.ca_key.exists() && !p.server.exists(), "nothing written");
    let err = issue(&p, "leaf").err().unwrap().to_string();
    assert!(err.contains("missing"), "{err}");
}

#[test]
fn issue_mints_a_distinct_pinned_identity_per_call() {
    let tmp = tempfile::tempdir().unwrap();
    let p = paths(tmp.path());
    ensure(&p).unwrap();
    let ca = read(&p.ca_cert);
    let a = issue(&p, "leaf").unwrap();
    let b = issue(&p, "leaf").unwrap();
    assert_ne!(a.fingerprint, b.fingerprint, "a new key every time");
    assert!(a.bundle.contains(ca.trim()));
    let id = tls_parse(&a.bundle);
    assert_eq!(
        id.fingerprint(),
        a.fingerprint,
        "the pin is the bundle's cert"
    );
    let boot = Identity::load(&p.bootstrap).unwrap().fingerprint();
    assert_ne!(a.fingerprint, boot);
    assert_eq!(bootstrap_fingerprint(&p.bootstrap).unwrap(), Some(boot));
    assert_eq!(
        bootstrap_fingerprint(&tmp.path().join("absent.pem")).unwrap(),
        None
    );
}

fn tls_parse(pem: &str) -> Identity {
    crate::daemon::tls::parse(pem, Path::new("issued.pem")).unwrap()
}

#[test]
fn fingerprints_are_lowercase_sha256_hex() {
    assert_eq!(
        fingerprint(b""),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    let f = fingerprint(b"x");
    assert_eq!(f.len(), FINGERPRINT_PREFIX.len() + 64);
}

#[test]
fn write_secret_atomic_replaces_whole_and_private() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("host.pem");
    write_secret_atomic(&f, "one").unwrap();
    write_secret_atomic(&f, "two").unwrap();
    assert_eq!(read(&f), "two");
    let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, FILE_MODE);
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
}

#[test]
fn missing_and_empty_bundles_are_explained() {
    let tmp = tempfile::tempdir().unwrap();
    let err = read_pem(&tmp.path().join("home.pem"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("export-creds"), "{err}");
    std::fs::write(tmp.path().join("e.pem"), "").unwrap();
    assert!(
        read_pem(&tmp.path().join("e.pem"))
            .unwrap_err()
            .to_string()
            .contains("no PEM")
    );
}
