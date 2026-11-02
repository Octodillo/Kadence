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

    /// A point in the log, for [`OwnChange::mark`].
    pub(crate) fn mark(&self) -> usize {
        self.events().len()
    }

    fn has(&self, change: &OwnChange) -> bool {
        self.events().get(change.mark..).is_some_and(|after| {
            after
                .iter()
                .any(|(l, m)| *l == change.link && *m == change.made)
        })
    }

    /// Whether every change in `own` has been notified.
    pub(crate) fn has_all(&self, own: &[OwnChange]) -> bool {
        own.iter().all(|c| self.has(c))
    }

    /// The links now: the log folded in order, then the changes in `own`
    /// it does not have yet (the server stalled before notifying them).
    pub(crate) fn links(&self, own: &[OwnChange]) -> Vec<Link> {
        let mut links: Vec<Link> = Vec::new();
        let mut apply = |link: &Link, made: bool| {
            links.retain(|l| l != link);
            if made {
                links.push(link.clone());
            }
        };
        for (link, made) in self.events().iter() {
            apply(link, *made);
        }
        for change in own.iter().filter(|c| !self.has(c)) {
            apply(&change.link, change.made);
        }
        links
    }
}

/// A link we made or removed while running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnChange {
    pub(crate) made: bool,
    pub(crate) link: Link,
    /// [`LinkLog::mark`] from just before the call: the server may notify a
    /// change before the call returns.
    pub(crate) mark: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(a: &str, b: &str) -> Link {
        (a.into(), b.into())
    }

    fn own(log: &LinkLog, made: bool, l: &Link) -> OwnChange {
        OwnChange {
            made,
            link: l.clone(),
            mark: log.mark(),
        }
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
        let log = LinkLog::default();
        let a = link("us:out", "hw:in");
        let ours = own(&log, true, &a);
        log.record(a.clone(), true);
        log.record(a.clone(), false);
        assert!(log.has_all(std::slice::from_ref(&ours)));
        assert!(log.links(&[ours]).is_empty());
    }

    #[test]
    fn our_change_not_notified_yet_is_kept() {
        let log = LinkLog::default();
        let a = link("us:out", "hw:in");
        let made = own(&log, true, &a);
        assert!(!log.has_all(std::slice::from_ref(&made)));
        assert_eq!(
            log.links(std::slice::from_ref(&made)),
            std::slice::from_ref(&a)
        );
        // A removal not notified yet undoes a notified link.
        log.record(a.clone(), true);
        let removed = own(&log, false, &a);
        assert!(log.links(&[made, removed]).is_empty());
    }

    #[test]
    fn an_older_matching_notification_does_not_count() {
        let log = LinkLog::default();
        let a = link("us:out", "hw:in");
        log.record(a.clone(), true);
        log.record(a.clone(), false);
        // Made again by us; the earlier "made" is from before the call.
        let again = own(&log, true, &a);
        assert!(!log.has_all(std::slice::from_ref(&again)));
        assert_eq!(log.links(&[again]), [a]);
    }
}
