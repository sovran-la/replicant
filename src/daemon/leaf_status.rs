//! A leaf's `status`: its subscriptions as home serves them, the plan (no
//! queue entry, no dry runs), the queue, and the open markers on both sides.

use super::{Leaf, Link};
use crate::Result;
use crate::daemon::wire::*;
use crate::plan::{self, HomeLink, LeafCtx, LeafStatus};

impl Leaf {
    pub(super) fn status(&self) -> Result<LeafStatus> {
        let cfg = self.cfg()?;
        let me = cfg.name()?;
        let link = match self.link(&cfg)? {
            Link::Host(r) => r,
            // Not enrolled yet: what home said about it is the whole status.
            Link::Bootstrap(_, state) => {
                let (home, enrollment, pending_paths) = match state {
                    Collected::Pending { home, paths } => (home, Enrollment::Pending, paths),
                    Collected::Unknown { home } => (home, Enrollment::Unknown, vec![]),
                    Collected::Issued { .. } => unreachable!("link() keeps an issued bundle"),
                };
                return Ok(LeafStatus {
                    me,
                    home,
                    enrollment,
                    cap: None,
                    paths: vec![],
                    except: vec![],
                    as_local: cfg.as_local,
                    pending_paths,
                    plan: None,
                    queue: vec![],
                    markers: vec![],
                });
            }
        };
        let view = plan::subscriptions(&link)?;
        let (hd, state) = (cfg.home_dir()?, self.dirs.state());
        let planned = match view.enrollment {
            Enrollment::Enrolled => Some(plan::plan(
                &link,
                LeafCtx {
                    me: &me,
                    home_dir: &hd,
                    cfg: &cfg,
                    state_dir: &state,
                    plane: plan::Plane::default(),
                    accept_rollback: &[],
                    resolve: &[],
                },
                false,
                false,
            )?),
            _ => None,
        };
        let (queue, home_markers) = match link.call(HomeRequest::FleetState)? {
            HomeReply::Fleet(f) => (f.queue, f.markers),
            _ => (vec![], vec![]),
        };
        let markers = plan::open_markers(
            &me,
            &view.home,
            crate::marker::store(&state).all()?,
            &home_markers,
            &view.specs,
            view.marker_alert_secs,
            crate::now_secs(),
        );
        let except = view.specs.iter().flat_map(|s| s.except.clone()).collect();
        Ok(LeafStatus {
            me,
            home: view.home,
            enrollment: view.enrollment,
            cap: view.cap,
            paths: view.specs.into_iter().map(|s| s.rel).collect(),
            except,
            as_local: cfg.as_local,
            pending_paths: view.pending_paths,
            plan: planned,
            queue,
            markers,
        })
    }
}
