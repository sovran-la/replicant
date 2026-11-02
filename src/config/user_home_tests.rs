//! Review N3: no test can fall through to the real `$HOME`. Every way a
//! config reaches [`super::user_home`] panics under `cfg(test)`.

use super::*;

#[test]
#[should_panic(expected = "user_home() called in a test")]
fn user_home_panics_in_a_test() {
    let _ = user_home();
}

#[test]
#[should_panic(expected = "user_home() called in a test")]
fn a_leaf_config_without_home_dir_panics_in_a_test() {
    let cfg = LeafConfig {
        name: Some("leaf".into()),
        home_dir: None,
        home: HomeEntry {
            name: "home".into(),
            address: "127.0.0.1:1".into(),
            credentials: None,
            rsync_host: None,
            rsync_timeout_secs: None,
        },
        as_local: Default::default(),
    };
    let _ = cfg.home_dir();
}

#[test]
#[should_panic(expected = "user_home() called in a test")]
fn a_fleet_toml_without_home_dir_panics_in_a_test() {
    let text = "[daemon]\nname = \"home\"\nlisten = \"127.0.0.1:0\"\n";
    let _ = Fleet::parse(text, Path::new("/nonexistent/fleet.toml"));
}

#[test]
fn a_config_that_names_its_home_dir_never_asks() {
    let tmp = tempfile::tempdir().unwrap();
    let text = format!(
        "[daemon]\nname = \"home\"\nhome_dir = \"{}\"\nlisten = \"127.0.0.1:0\"\n",
        tmp.path().display()
    );
    let f = Fleet::parse(&text, Path::new("/nonexistent/fleet.toml")).unwrap();
    assert_eq!(f.home_dir, tmp.path());
}
