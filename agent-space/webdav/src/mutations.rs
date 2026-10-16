//! Fair, cancellation-safe reservations for overlapping DAV paths. Each request
//! reserves source and destination together, so opposite moves cannot deadlock.
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Default)]
pub struct Mutations {
    entries: Mutex<Vec<Arc<Vec<Vec<u8>>>>>,
    changed: Notify,
}
pub struct Guard {
    owner: Arc<Mutations>,
    paths: Arc<Vec<Vec<u8>>>,
}
fn overlaps(a: &[u8], b: &[u8]) -> bool {
    a == b
        || a.is_empty()
        || b.is_empty()
        || (a.starts_with(b) && a.get(b.len()) == Some(&b'/'))
        || (b.starts_with(a) && b.get(a.len()) == Some(&b'/'))
}
impl Mutations {
    pub async fn acquire(self: Arc<Self>, paths: Vec<Vec<u8>>) -> Guard {
        let paths = Arc::new(paths);
        self.entries.lock().unwrap().push(paths.clone());
        // Own the queue entry before awaiting: timeout/drop removes waiters too.
        let guard = Guard {
            owner: self.clone(),
            paths: paths.clone(),
        };
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let blocked = self
                .entries
                .lock()
                .unwrap()
                .iter()
                .take_while(|entry| !Arc::ptr_eq(entry, &paths))
                .any(|entry| entry.iter().any(|a| paths.iter().any(|b| overlaps(a, b))));
            if !blocked {
                return guard;
            }
            changed.await;
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.owner
            .entries
            .lock()
            .unwrap()
            .retain(|entry| !Arc::ptr_eq(entry, &self.paths));
        self.owner.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reservations_are_fair_atomic_and_cancel_safe() {
        let gate = Arc::new(Mutations::default());
        let first = gate.clone().acquire(vec![b"dir/a".to_vec()]).await;
        let parent = gate.clone().acquire(vec![b"dir".to_vec()]);
        tokio::pin!(parent);
        assert!(futures::poll!(&mut parent).is_pending());
        // A later child cannot starve the earlier parent, but an unrelated tree progresses.
        let child = gate.clone().acquire(vec![b"dir/b".to_vec()]);
        tokio::pin!(child);
        assert!(futures::poll!(&mut child).is_pending());
        let unrelated = gate.clone().acquire(vec![b"dir2/a".to_vec()]).await;
        drop(first);
        let parent = parent.await;
        assert!(futures::poll!(&mut child).is_pending());
        drop(parent);
        drop(child.await);
        drop(unrelated);
        assert!(gate.entries.lock().unwrap().is_empty());
        let first = gate
            .clone()
            .acquire(vec![b"a".to_vec(), b"b".to_vec()])
            .await;
        let mut reverse = Box::pin(gate.clone().acquire(vec![b"b".to_vec(), b"a".to_vec()]));
        assert!(futures::poll!(&mut reverse).is_pending());
        drop(reverse);
        drop(first);
        assert!(gate.entries.lock().unwrap().is_empty());
        drop(gate.clone().acquire(vec![Vec::new()]).await);
    }
}
