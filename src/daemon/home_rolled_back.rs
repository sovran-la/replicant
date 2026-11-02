//! Rolled-back pushes, as the pushing host sees them (review A2): home
//! reports each of a host's pushes it rolled back until that host and home
//! agree on the unit again, so the host's sync holds the unit rather than
//! pulling home's restored copy over the work it pushed.

use super::Home;
use super::backup::{Push, pushes};
use crate::Result;
use crate::decide::RolledBackPush;

impl Home {
    /// `host`'s rolled-back pushes not yet settled, the newest per unit.
    pub(super) fn rolled_back_for(&self, host: &str) -> Result<Vec<RolledBackPush>> {
        let mut held: Vec<Push> = pushes(&self.dirs.state())
            .all::<Push>()?
            .into_iter()
            .filter(|p| p.host == host && p.rollback_settled_at.is_none())
            .filter(|p| p.rolled_back_at.is_some())
            .collect();
        held.sort_by(|a, b| {
            (a.unit.as_str(), b.rolled_back_at, &b.id).cmp(&(
                b.unit.as_str(),
                a.rolled_back_at,
                &a.id,
            ))
        });
        held.dedup_by(|later, first| later.unit == first.unit);
        Ok(held
            .into_iter()
            .map(|p| RolledBackPush {
                at: p.rolled_back_at.unwrap_or_default(),
                id: p.id,
                unit: p.unit,
            })
            .collect())
    }

    /// `host` and home agree on `rel` again: every rollback of `host`'s
    /// pushes of exactly `rel` is settled.
    pub(super) fn settle_rollbacks(&self, host: &str, rel: &str) -> Result<()> {
        let store = pushes(&self.dirs.state());
        let now = crate::now_secs();
        for mut p in store.all::<Push>()? {
            let held = p.rolled_back_at.is_some() && p.rollback_settled_at.is_none();
            if held && p.host == host && p.unit == rel {
                p.rollback_settled_at = Some(now);
                store.put(&p)?;
            }
        }
        Ok(())
    }
}
