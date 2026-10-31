//! The daemon's writes to fleet.toml, through toml_edit so comments survive.
//!
//! Every function here reads the file fresh, changes one table, and writes it
//! back atomically. The caller (home's daemon, under its fleet lock) reloads
//! the [`super::Fleet`] afterwards, so the in-memory view is always what the
//! file says.

use super::{HostEntry, Pending, Subscription};
use crate::{Error, Result};
use std::path::Path;
use toml_edit::{Array, DocumentMut, Item, Table, value};

fn load(path: &Path) -> Result<DocumentMut> {
    let text = std::fs::read_to_string(path)?;
    text.parse::<DocumentMut>()
        .map_err(|e| Error::Config(format!("{} is not valid TOML: {e}", path.display())))
}

fn store(path: &Path, doc: &DocumentMut) -> Result<()> {
    super::write_atomic(path, &doc.to_string())
}

/// The `[name]` parent table, created implicit so it renders as `[name.host]`.
fn parent<'d>(doc: &'d mut DocumentMut, name: &str) -> Result<&'d mut Table> {
    let item = doc.entry(name).or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    item.as_table_mut()
        .ok_or_else(|| Error::Config(format!("[{name}] in fleet.toml is not a table")))
}

/// The `[name.host]` table, created if absent, existing keys and comments kept.
fn child<'d>(doc: &'d mut DocumentMut, name: &str, host: &str) -> Result<&'d mut Table> {
    let item = parent(doc, name)?
        .entry(host)
        .or_insert_with(|| Item::Table(Table::new()));
    item.as_table_mut()
        .ok_or_else(|| Error::Config(format!("[{name}.{host}] in fleet.toml is not a table")))
}

fn array(list: &[String]) -> Item {
    let mut a = Array::new();
    for p in list {
        a.push(p.as_str());
    }
    value(a)
}

pub fn set_subscription(path: &Path, host: &str, sub: &Subscription) -> Result<()> {
    let mut doc = load(path)?;
    let t = child(&mut doc, "subscriptions", host)?;
    t.insert("paths", array(&sub.paths));
    if sub.except.is_empty() {
        t.remove("except");
    } else {
        t.insert("except", array(&sub.except));
    }
    store(path, &doc)
}

pub fn set_pending(path: &Path, host: &str, p: &Pending) -> Result<()> {
    let mut doc = load(path)?;
    let t = child(&mut doc, "pending", host)?;
    t.insert("home_dir", value(p.home_dir.display().to_string()));
    t.insert("paths", array(&p.paths));
    t.insert("requested_at", value(p.requested_at as i64));
    store(path, &doc)
}

pub fn remove_pending(path: &Path, host: &str) -> Result<()> {
    let mut doc = load(path)?;
    let now_empty = match doc.get_mut("pending").and_then(Item::as_table_mut) {
        Some(t) => {
            t.remove(host);
            t.is_empty()
        }
        None => false,
    };
    if now_empty {
        doc.remove("pending");
    }
    store(path, &doc)
}

pub fn set_host(path: &Path, host: &str, entry: &HostEntry) -> Result<()> {
    let mut doc = load(path)?;
    let t = child(&mut doc, "hosts", host)?;
    t.insert("home_dir", value(entry.home_dir.display().to_string()));
    match &entry.allow {
        Some(allow) => {
            t.insert("allow", array(allow));
        }
        None => {
            t.remove("allow");
        }
    }
    store(path, &doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Fleet;
    use std::path::PathBuf;

    const BASE: &str = "# hand-written header\n[daemon]\nname = \"home\"\nhome_dir = \"/h\"\n\n\
                        [roots.work]\npath = \"/h/work\" # the big one\n";

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("fleet.toml");
        std::fs::write(&p, BASE).unwrap();
        (tmp, p)
    }

    #[test]
    fn subscription_round_trips_and_keeps_comments() {
        let (_t, p) = fixture();
        let sub = Subscription {
            paths: vec!["work/a".into()],
            except: vec!["work/a/x".into()],
        };
        set_subscription(&p, "leaf", &sub).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("# hand-written header") && text.contains("# the big one"),
            "{text}"
        );
        assert!(text.contains("[subscriptions.leaf]"), "{text}");
        let f = Fleet::load(&p).unwrap();
        assert_eq!(f.subscription("leaf"), sub);

        set_subscription(
            &p,
            "leaf",
            &Subscription {
                paths: vec!["work".into()],
                except: vec![],
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("except"), "empty except is removed: {text}");
    }

    #[test]
    fn pending_set_and_remove() {
        let (_t, p) = fixture();
        let pend = Pending {
            home_dir: "/Users/x".into(),
            paths: vec!["work".into()],
            requested_at: 42,
        };
        set_pending(&p, "new", &pend).unwrap();
        assert_eq!(Fleet::load(&p).unwrap().pending["new"], pend);
        remove_pending(&p, "new").unwrap();
        let f = Fleet::load(&p).unwrap();
        assert!(f.pending.is_empty());
        assert!(!std::fs::read_to_string(&p).unwrap().contains("[pending"));
        remove_pending(&p, "absent").unwrap();
    }

    #[test]
    fn host_set_with_and_without_cap() {
        let (_t, p) = fixture();
        let capped = HostEntry {
            home_dir: "/Users/x".into(),
            allow: Some(vec!["work/t".into()]),
        };
        set_host(&p, "t16", &capped).unwrap();
        assert_eq!(Fleet::load(&p).unwrap().hosts["t16"], capped);
        let open = HostEntry {
            home_dir: "/Users/x".into(),
            allow: None,
        };
        set_host(&p, "t16", &open).unwrap();
        assert_eq!(Fleet::load(&p).unwrap().hosts["t16"], open);
    }

    #[test]
    fn non_table_section_is_an_error_not_a_clobber() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("fleet.toml");
        std::fs::write(&p, "subscriptions = 3\n").unwrap();
        let sub = Subscription::default();
        assert!(set_subscription(&p, "x", &sub).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "subscriptions = 3\n");
    }
}
