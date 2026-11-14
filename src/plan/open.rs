//! Open markers as `status` reports them: every interrupted (or running)
//! sync this leaf can see, on either side, with its age. PLAN.md,
//! "Interrupted sync": a marker older than home's `marker_alert_mins` is
//! reported as interrupted; a younger one may still be running.

use crate::marker::Marker;
use crate::units::SubSpec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenMarker {
    pub marker: Marker,
    /// The side that holds it: this leaf's name (a pull) or home's (a push).
    pub on: String,
    pub age_secs: u64,
    /// Older than the alert age: not running any more.
    pub alert: bool,
}

/// This leaf's own markers, and home's over anything it subscribes to
/// (its own pushes, and other hosts' that quarantine its paths).
pub fn open_markers(
    me: &str,
    home: &str,
    leaf: Vec<Marker>,
    home_markers: &[Marker],
    specs: &[SubSpec],
    alert_secs: u64,
    now: u64,
) -> Vec<OpenMarker> {
    let mk = |marker: Marker, on: &str| {
        let age_secs = now.saturating_sub(marker.started_at);
        OpenMarker {
            marker,
            on: on.into(),
            age_secs,
            alert: age_secs >= alert_secs,
        }
    };
    let mut out: Vec<OpenMarker> = leaf.into_iter().map(|m| mk(m, me)).collect();
    out.extend(
        home_markers
            .iter()
            .filter(|m| specs.iter().any(|s| m.covers(&s.rel)))
            .map(|m| mk(m.clone(), home)),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marker::Direction;
    use crate::record::RECORD_VERSION;
    use crate::state::UnitState;

    fn m(rel: &str, host: &str, at: u64) -> Marker {
        Marker {
            version: RECORD_VERSION,
            rel: rel.into(),
            direction: Direction::Push,
            host: host.into(),
            source_state: UnitState::Empty,
            started_at: at,
            push: None,
            backup: None,
        }
    }

    fn spec(rel: &str) -> SubSpec {
        SubSpec {
            rel: rel.into(),
            ..Default::default()
        }
    }

    #[test]
    fn ages_alerts_and_only_home_markers_over_this_leafs_paths() {
        let leaf = vec![m("work/a", "leaf", 1000)];
        let home = [
            m("work/b", "other", 9000),
            m("rust/x", "other", 1000),
            m("work", "leaf", 9500),
        ];
        let got = open_markers("leaf", "home", leaf, &home, &[spec("work")], 3600, 10_000);
        let seen: Vec<(&str, &str, u64, bool)> = got
            .iter()
            .map(|o| (o.marker.rel.as_str(), o.on.as_str(), o.age_secs, o.alert))
            .collect();
        assert_eq!(
            seen,
            [
                ("work/a", "leaf", 9000, true),
                ("work/b", "home", 1000, false),
                ("work", "home", 500, false)
            ]
        );
        let edge = open_markers("leaf", "home", vec![m("w", "leaf", 0)], &[], &[], 10, 10);
        assert!(edge[0].alert, "at the alert age exactly");
        let future = open_markers("leaf", "home", vec![m("w", "leaf", 50)], &[], &[], 10, 10);
        assert_eq!(future[0].age_secs, 0, "a clock behind the marker is age 0");
    }
}
