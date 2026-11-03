//! Review P3: a root exclude with a `/` is relative to the **root**, not to
//! the unit it is applied in. `agents/state` on the `.sovran` root means
//! `.sovran/agents/state` for a host subscribed to `.sovran`, to
//! `.sovran/agents`, or to anything else under the root.
//!
//! Matching happens inside one unit, against paths below its root, so each such pattern is rewritten per unit by stripping the
//! unit's root-relative prefix from its front, component by component:
//!
//! - in `.sovran/agents`, `agents/state` becomes the anchored `/state`;
//! - in `.sovran/agents/state` (or below), it covers the whole unit;
//! - in `.sovran/x`, it can never match, so it is dropped.
//!
//! A wildcard component strips a prefix component it matches; `**` strips
//! any number of them, so `a/**/b` in unit `a/x` is `/**/b`. Bare names and
//! `**/…` patterns already match at any depth and are not rewritten. The
//! rewritten patterns are ordinary [`Parsed`] values.

use super::{Comp, Parsed, comp_glob};

/// A root-relative pattern, as it applies inside one unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::exclude) enum Rebased {
    /// Not root-relative, or the unit is the root: use it as written.
    Same,
    /// Anchored at the unit; empty when the pattern cannot match inside it.
    Within(Vec<Parsed>),
    /// The pattern excludes the unit itself, or everything inside it.
    Covers,
}

impl Parsed {
    /// A path pattern written without a leading `/` or `**/`.
    fn is_root_relative(&self) -> bool {
        !self.anchored && self.comps.len() > 1 && self.comps[0] != Comp::Globstar
    }

    /// This pattern inside the unit at `prefix`, the unit's path below its
    /// root (`""` for the root itself).
    pub(in crate::exclude) fn rebase(&self, prefix: &str) -> Rebased {
        if prefix.is_empty() || !self.is_root_relative() {
            return Rebased::Same;
        }
        let unit: Vec<&str> = prefix.split('/').collect();
        let mut tails = Vec::new();
        strip(&self.comps, &unit, &mut tails);
        // Nothing left: it matched the unit or an ancestor. A bare `**` left:
        // everything inside the unit (`cache/**` in unit `cache`).
        if tails.iter().any(|t| t.is_empty() || *t == [Comp::Globstar]) {
            return Rebased::Covers;
        }
        let mut out: Vec<Parsed> = Vec::new();
        for comps in tails {
            let p = Parsed {
                comps,
                anchored: true,
                dir_only: self.dir_only,
            };
            if !out.contains(&p) {
                out.push(p);
            }
        }
        Rebased::Within(out)
    }
}

/// `/**`: everything inside the unit, for a unit a pattern [`Rebased::Covers`].
pub(in crate::exclude) fn everything() -> Parsed {
    Parsed {
        comps: vec![Comp::Globstar],
        anchored: true,
        dir_only: false,
    }
}

/// Every way `comps` can consume all of `unit` from the front, as what is
/// left of the pattern.
fn strip(comps: &[Comp], unit: &[&str], out: &mut Vec<Vec<Comp>>) {
    let Some((name, deeper)) = unit.split_first() else {
        out.push(comps.to_vec());
        return;
    };
    match comps.split_first() {
        None => out.push(Vec::new()),
        Some((Comp::Globstar, rest)) => {
            strip(rest, unit, out);
            strip(comps, deeper, out);
        }
        Some((c, rest)) => {
            if crate::exclude::compile(&comp_glob(c))
                .compile_matcher()
                .is_match(name)
            {
                strip(rest, deeper, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    fn rebased(raw: &str, prefix: &str) -> Result<Vec<String>, Rebased> {
        match parse(raw).unwrap().rebase(prefix) {
            Rebased::Within(ps) => Ok(ps
                .iter()
                .map(|p| format!("/{}{}", p.glob(), if p.dir_only { "/" } else { "" }))
                .collect()),
            other => Err(other),
        }
    }

    #[test]
    fn a_literal_prefix_is_stripped_dropped_or_covering() {
        assert_eq!(rebased("agents/state", "agents"), Ok(vec!["/state".into()]));
        assert_eq!(rebased("agents/state", "x"), Ok(vec![]));
        assert_eq!(
            rebased("agents/state", "agents/state"),
            Err(Rebased::Covers)
        );
        assert_eq!(
            rebased("agents/state", "agents/state/x"),
            Err(Rebased::Covers)
        );
        assert_eq!(rebased("a/b/c/", "a"), Ok(vec!["/b/c/".into()]));
        assert_eq!(rebased("a/b/c/", "a/b"), Ok(vec!["/c/".into()]));
    }

    #[test]
    fn wildcards_strip_what_they_match() {
        assert_eq!(rebased("q/*/m", "q"), Ok(vec!["/*/m".into()]));
        assert_eq!(rebased("q/*/m", "q/x"), Ok(vec!["/m".into()]));
        assert_eq!(rebased("q/*/m", "q/x/m"), Err(Rebased::Covers));
        assert_eq!(rebased("v[0-9]/x", "v1"), Ok(vec!["/x".into()]));
        assert_eq!(rebased("v[0-9]/x", "vx"), Ok(vec![]));
        assert_eq!(rebased("src/*.rs", "lib"), Ok(vec![]));
        // A literal `*` in the name is matched literally.
        assert_eq!(rebased("\\*lit/x", "*lit"), Ok(vec!["/x".into()]));
        assert_eq!(rebased("\\*lit/x", "alit"), Ok(vec![]));
    }

    #[test]
    fn globstar_strips_any_number_of_components() {
        assert_eq!(rebased("a/**/b", "a"), Ok(vec!["/**/b".into()]));
        assert_eq!(rebased("a/**/b", "a/x"), Ok(vec!["/**/b".into()]));
        assert_eq!(rebased("a/**/b", "a/x/b"), Err(Rebased::Covers));
        assert_eq!(rebased("a/**/b", "a/b/y"), Err(Rebased::Covers));
        assert_eq!(rebased("cache/**", "cache"), Err(Rebased::Covers));
        assert_eq!(rebased("cache/**", "cache/sub"), Err(Rebased::Covers));
        assert_eq!(rebased("cache/**", "other"), Ok(vec![]));
        assert_eq!(
            rebased("a/**/b/c", "a/x/b"),
            Ok(vec!["/c".into(), "/**/b/c".into()])
        );
    }

    #[test]
    fn names_globstar_patterns_and_the_root_are_left_alone() {
        for raw in [
            "target",
            "*.log",
            "build/",
            "**/agents/state",
            "**",
            "**/x/**/y",
        ] {
            assert_eq!(parse(raw).unwrap().rebase("agents"), Rebased::Same, "{raw}");
        }
        assert_eq!(parse("agents/state").unwrap().rebase(""), Rebased::Same);
    }
}
