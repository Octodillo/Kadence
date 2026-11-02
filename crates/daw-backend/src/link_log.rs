//! What [`JackBackend`](crate::JackBackend) knows about its ports' links
//! while running, so `stop` can save them: deactivating a JACK client drops
//! every link it has.
//!
//! Asking the server (`jack_port_get_connections`) is not good enough at
//! that moment. jackd answers from its current graph, which it swaps in only
//! when a cycle starts on time, so under load the answer can lag a link
//! change by many cycles, and a patchbay's change made just before the stop
//! is lost. The server does send every client a connect or disconnect
//! notification as it makes the change, in the order it makes them. A
//! [`LinkLog`] keeps those, and folding it gives the links as they are.
//!
//! Our own changes are also known the moment the call returns, before their
//! notification arrives. Each is kept as an [`OwnChange`] with the log's
//! length from before the call: once a matching notification is in the log
//! after that point, the log has it; until then it is applied on top.

use std::sync::{Mutex, MutexGuard};

/// A link from an output port to an input port, by full name.
pub(crate) type Link = (String, String);

/// Link changes the server notified, oldest first: each link with whether
/// it was made (`true`) or removed.
#[derive(Default)]
pub(crate) struct LinkLog {
    events: Mutex<Vec<(Link, bool)>>,
}

impl LinkLog {
    fn events(&self) -> MutexGuard<'_, Vec<(Link, bool)>> {
        // A panic while holding the lock leaves a whole Vec; keep using it.
        self.events.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Called from the notification thread, never the audio thread.
    pub(crate) fn record(&self, link: Link, made: bool) {
        self.events().push((link, made));
    }

    /// Forgets everything, and starts over from `links`.
    pub(crate) fn reset(&self, links: Vec<Link>) {
        let mut events = self.events();
        events.clear();
        events.extend(links.into_iter().map(|l| (l, true)));
    }

    /// Records the end of every link of `port`, which we are removing: the
    /// server may not tell us, since the port is no longer ours by the time
    /// it would.
    pub(crate) fn forget_port(&self, port: &str) {
        let mut events = self.events();
        let gone: Vec<Link> = fold(&events, &[])
            .into_iter()
            .filter(|(a, b)| a == port || b == port)
            .collect();
        events.extend(gone.into_iter().map(|l| (l, false)));
    }

    /// Call before we make or remove a link while running, with our changes
    /// so far; the result is the new change's [`OwnChange::mark`]. Drops the
    /// changes the log has, and when that is all of them, compacts the log
    /// so it doesn't grow for as long as we run.
    pub(crate) fn begin_change(&self, recent: &mut Vec<OwnChange>) -> usize {
        let mut events = self.events();
        recent.retain(|c| !has_in(&events, c));
        if recent.is_empty() {
            let links = fold(&events, &[]);
            *events = links.into_iter().map(|l| (l, true)).collect();
        }
        events.len()
    }

    /// Whether every change in `own` has been notified.
    pub(crate) fn has_all(&self, own: &[OwnChange]) -> bool {
        let events = self.events();
        own.iter().all(|c| has_in(&events, c))
    }

    /// The links now: the log folded in order, then the changes in `own`
    /// it does not have yet (the server stalled before notifying them).
    pub(crate) fn links(&self, own: &[OwnChange]) -> Vec<Link> {
        fold(&self.events(), own)
    }
}

fn has_in(events: &[(Link, bool)], change: &OwnChange) -> bool {
    events.get(change.mark..).is_some_and(|after| {
        after
            .iter()
            .any(|(l, m)| *l == change.link && *m == change.made)
    })
}

/// `events` folded in order, then the changes in `own` not in them.
fn fold(events: &[(Link, bool)], own: &[OwnChange]) -> Vec<Link> {
    let mut links: Vec<Link> = Vec::new();
    let mut apply = |link: &Link, made: bool| {
        links.retain(|l| l != link);
        if made {
            links.push(link.clone());
        }
    };
    for (link, made) in events {
        apply(link, *made);
    }
    for change in own.iter().filter(|c| !has_in(events, c)) {
        apply(&change.link, change.made);
    }
    links
}

/// A link we made or removed while running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnChange {
    pub(crate) made: bool,
    pub(crate) link: Link,
    /// [`LinkLog::begin_change`] from just before the call: the server may notify a
    /// change before the call returns.
    pub(crate) mark: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(a: &str, b: &str) -> Link {
        (a.into(), b.into())
    }

    /// Our change to `l`, made now, the way the backend records it.
    fn ours(log: &LinkLog, recent: &mut Vec<OwnChange>, made: bool, l: &Link) {
        let mark = log.begin_change(recent);
        recent.push(OwnChange {
            made,
            link: l.clone(),
            mark,
        });
    }

    #[test]
    fn folds_notifications_in_order() {
        let log = LinkLog::default();
        let (a, b) = (link("us:out", "hw:in"), link("hw:cap", "us:in"));
        log.reset(vec![a.clone()]);
        log.record(b.clone(), true);
        log.record(a.clone(), false);
        log.record(a.clone(), true);
        log.record(b.clone(), false);
        assert_eq!(log.links(&[]), [a]);
    }

    #[test]
    fn a_patchbay_change_after_ours_wins() {
        let (log, mut recent) = (LinkLog::default(), Vec::new());
        let a = link("us:out", "hw:in");
        ours(&log, &mut recent, true, &a);
        log.record(a.clone(), true);
        log.record(a.clone(), false);
        assert!(log.has_all(&recent));
        assert!(log.links(&recent).is_empty());
    }

    #[test]
    fn our_changes_not_notified_yet_are_kept() {
        let (log, mut recent) = (LinkLog::default(), Vec::new());
        let a = link("us:out", "hw:in");
        ours(&log, &mut recent, true, &a);
        assert!(!log.has_all(&recent));
        assert_eq!(log.links(&recent), std::slice::from_ref(&a));
        // A removal not notified yet undoes a notified link.
        log.record(a.clone(), true);
        ours(&log, &mut recent, false, &a);
        assert_eq!(recent.len(), 1, "the notified change is dropped");
        assert!(log.links(&recent).is_empty());
    }

    #[test]
    fn an_older_matching_notification_does_not_count() {
        let (log, mut recent) = (LinkLog::default(), Vec::new());
        let (a, b) = (link("us:out", "hw:in"), link("us:out", "hw:in2"));
        // Pending, so the log is not compacted below.
        ours(&log, &mut recent, true, &b);
        log.record(a.clone(), true);
        log.record(a.clone(), false);
        // Made again by us; the earlier "made" is from before the call.
        ours(&log, &mut recent, true, &a);
        assert!(!log.has_all(&recent));
        assert_eq!(log.links(&recent), [b, a]);
    }

    #[test]
    fn the_log_is_compacted_once_nothing_is_pending() {
        let (log, mut recent) = (LinkLog::default(), Vec::new());
        let a = link("us:out", "hw:in");
        for _ in 0..100 {
            ours(&log, &mut recent, true, &a);
            log.record(a.clone(), true);
            ours(&log, &mut recent, false, &a);
            log.record(a.clone(), false);
        }
        ours(&log, &mut recent, true, &a);
        assert_eq!(recent.len(), 1);
        assert!(log.events().is_empty(), "{:?}", log.events());
    }

    #[test]
    fn a_forgotten_port_loses_its_links() {
        let log = LinkLog::default();
        let (a, b) = (link("src:out", "us:pads"), link("us:out", "hw:in"));
        log.record(a, true);
        log.record(b.clone(), true);
        log.forget_port("us:pads");
        assert_eq!(log.links(&[]), [b]);
    }
}
