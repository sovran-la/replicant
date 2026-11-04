//! What a unit's transfer would change, or did, on its target: the shape
//! every consumer of a dry run reads (the plan's summary, the guardrails,
//! the push backup and manifest, the journal). Its producer is the data
//! plane; nothing here depends on how the bytes move.

/// What happens to one target path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    Create,
    Update,
    /// Metadata only (permissions).
    Attr,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    /// Relative to the unit, without a trailing slash.
    pub path: String,
    pub is_dir: bool,
}

/// A unit's changes, as a dry run reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Itemized {
    pub changes: Vec<Change>,
    /// Anything the producer said that is not a change.
    pub notes: Vec<String>,
}

impl Itemized {
    /// Files (and links) created or updated.
    pub fn files_changed(&self) -> usize {
        self.count(|c| !c.is_dir && matches!(c.kind, ChangeKind::Create | ChangeKind::Update))
    }
    /// Files (and links) deleted.
    pub fn deleted(&self) -> usize {
        self.count(|c| !c.is_dir && c.kind == ChangeKind::Delete)
    }
    /// Whether anything at all would change.
    pub fn is_empty(&self) -> bool {
        self.changes
            .iter()
            .all(|c| c.kind == ChangeKind::Attr && c.is_dir)
    }
    fn count(&self, f: impl Fn(&Change) -> bool) -> usize {
        self.changes.iter().filter(|c| f(c)).count()
    }
    /// Paths whose bytes would be replaced or removed on the target.
    pub fn touched(&self) -> impl Iterator<Item = &str> {
        self.changes
            .iter()
            .filter(|c| !c.is_dir && c.kind != ChangeKind::Attr)
            .map(|c| c.path.as_str())
    }
}
