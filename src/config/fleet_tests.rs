use super::*;

const SAMPLE: &str = r#"
# thistledown
[daemon]
listen = "127.0.0.1:7750"
name = "thistledown"
home_dir = "/home/morgan"
stale_after_hours = 24

[roots.work]
path    = "/home/morgan/work"
exclude = ["target", ".build", "node_modules", "bin", ".DS_Store"]

[roots.claude]
path    = "/home/morgan/.claude"
exclude = ["projects", "*.jsonl"]

[hosts.birchbook]
home_dir = "/Users/morgan"

[hosts.twinoaks]
home_dir = "/Users/morgan"
allow    = ["work/tundra", "work/sequoia"]

[subscriptions.twinoaks]
paths = ["work/tundra", "work/sequoia"]

[pending.spruce]
home_dir = "/home/morgan"
paths = ["work"]
requested_at = 1700000000
"#;

fn parse(text: &str) -> Result<Fleet> {
    Fleet::parse(text, Path::new("fleet.toml"))
}

#[test]
fn parses_the_plan_example() {
    let f = parse(SAMPLE).unwrap();
    assert_eq!(f.name, "thistledown");
    assert_eq!(f.listen.as_deref(), Some("127.0.0.1:7750"));
    assert_eq!(f.home_dir, PathBuf::from("/home/morgan"));
    assert_eq!(f.stale_after, Duration::from_secs(24 * 3600));
    assert_eq!(f.lease, Duration::from_secs(DEFAULT_LEASE_SECS));
    assert_eq!(f.root_rels(), vec![".claude", "work"]);
    assert_eq!(f.root_for("work/tundra/x").unwrap().name, "work");
    assert!(f.root_for("work2").is_none());
    assert_eq!(
        f.hosts["twinoaks"].allow.as_deref(),
        Some(&["work/tundra".to_string(), "work/sequoia".into()][..])
    );
    assert_eq!(f.hosts["birchbook"].allow, None);
    assert_eq!(
        f.subscription("twinoaks").paths,
        vec!["work/tundra", "work/sequoia"]
    );
    assert_eq!(f.subscription("nobody"), Subscription::default());
    assert_eq!(f.pending["spruce"].requested_at, 1_700_000_000);
    assert_eq!(f.home_path("work/x"), PathBuf::from("/home/morgan/work/x"));
}

#[test]
fn root_outside_home_dir_is_refused() {
    let err = parse("[daemon]\nname=\"h\"\nhome_dir=\"/home/b\"\n[roots.x]\npath=\"/srv/x\"\n")
        .unwrap_err();
    assert!(
        err.to_string().contains("not inside home's home dir"),
        "{err}"
    );
}

#[test]
fn overlapping_roots_are_refused() {
    let err = parse(
        "[daemon]\nname=\"h\"\nhome_dir=\"/h\"\n[roots.a]\npath=\"/h/work\"\n[roots.b]\npath=\"/h/work/x\"\n",
    )
    .unwrap_err();
    assert!(err.to_string().contains("overlap"), "{err}");
}

#[test]
fn anchored_exclude_is_refused() {
    let err = parse(
        "[daemon]\nname=\"h\"\nhome_dir=\"/h\"\n[roots.a]\npath=\"/h/w\"\nexclude=[\"/x\"]\n",
    )
    .unwrap_err();
    assert!(err.to_string().contains("anchored"), "{err}");
}

/// A pattern rsync cannot read the same way is refused when fleet.toml loads,
/// not discovered later as a sync that moves what replicant thought excluded.
#[test]
fn untranslatable_exclude_is_refused_at_load() {
    for (pat, why) in [
        ("file?", "one byte"),
        ("{a,b}", "alternation"),
        (".git/**.lock", "**"),
    ] {
        let err = parse(&format!(
            "[daemon]\nname=\"h\"\nhome_dir=\"/h\"\n[roots.a]\npath=\"/h/w\"\nexclude=[{pat:?}]\n"
        ))
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("[roots.a] exclude") && err.contains(why),
            "{pat}: {err}"
        );
    }
    parse(
        "[daemon]\nname=\"h\"\nhome_dir=\"/h\"\n[roots.a]\npath=\"/h/w\"\n\
         exclude=[\"agents/state\", \"**/*.sock\", \"target\"]\n",
    )
    .expect("the plan's patterns load");
}

#[test]
fn unknown_keys_are_refused() {
    assert!(parse("[daemon]\nname=\"h\"\nhome_dir=\"/h\"\nlisten_typo=\"x\"\n").is_err());
}

#[test]
fn non_normalized_allow_is_refused() {
    let err = parse(
        "[daemon]\nname=\"h\"\nhome_dir=\"/h\"\n[hosts.a]\nhome_dir=\"/u\"\nallow=[\"work/../x\"]\n",
    )
    .unwrap_err();
    assert!(err.to_string().contains("allow"), "{err}");
}

#[test]
fn relative_home_dir_is_refused() {
    assert!(parse("[daemon]\nname=\"h\"\nhome_dir=\"h\"\n").is_err());
    assert!(parse("[daemon]\nname=\"h\"\nhome_dir=\"/h\"\n[hosts.a]\nhome_dir=\"u\"\n").is_err());
}

#[test]
fn lease_is_at_least_one_second() {
    let f = parse("[daemon]\nname=\"h\"\nhome_dir=\"/h\"\nlease_secs=0\n").unwrap();
    assert_eq!(f.lease, Duration::from_secs(1));
}
