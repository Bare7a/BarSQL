use std::collections::HashMap;
use std::sync::Mutex;

use barsql_db::Cancel;

use crate::lock;

// Keyed by tab or job id. New work under a key cancels the old, and ids are assigned under the same
// lock so id order matches cancel order.
#[derive(Default)]
pub(crate) struct Jobs {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    seq: u64,
    running: HashMap<String, Job>,
}

struct Job {
    id: u64,
    connection_id: String,
    cancel: Cancel,
}

impl Jobs {
    pub(crate) fn start(&self, key: &str, connection_id: &str) -> (u64, Cancel) {
        let mut state = lock(&self.state);
        if let Some(old) = state.running.remove(key) {
            old.cancel.cancel();
        }
        state.seq += 1;
        let id = state.seq;
        let cancel = Cancel::new();
        state
            .running
            .insert(key.to_string(), Job { id, connection_id: connection_id.to_string(), cancel: cancel.clone() });
        (id, cancel)
    }

    pub(crate) fn end(&self, key: &str, id: u64) {
        let mut state = lock(&self.state);
        if state.running.get(key).is_some_and(|job| job.id == id) {
            state.running.remove(key);
        }
    }

    pub(crate) fn cancel(&self, key: &str) -> bool {
        let job = lock(&self.state).running.remove(key);
        match job {
            Some(job) => {
                job.cancel.cancel();
                true
            }
            None => false,
        }
    }

    pub(crate) fn cancel_connection(&self, connection_id: &str) {
        let mut state = lock(&self.state);
        let keys: Vec<String> = state
            .running
            .iter()
            .filter(|(_, job)| job.connection_id == connection_id)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            if let Some(job) = state.running.remove(&key) {
                job.cancel.cancel();
            }
        }
    }

    pub(crate) fn cancel_all(&self) {
        for (_, job) in lock(&self.state).running.drain() {
            job.cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_job_supersedes_the_old_one_under_the_same_key() {
        let jobs = Jobs::default();
        let (first, first_cancel) = jobs.start("tab", "c1");
        let (second, second_cancel) = jobs.start("tab", "c1");
        assert!(second > first);
        assert!(first_cancel.is_cancelled());
        assert!(!second_cancel.is_cancelled());
        jobs.end("tab", first);
        assert!(jobs.cancel("tab"), "ending a superseded job must not unregister its successor");
        assert!(second_cancel.is_cancelled());
        assert!(!jobs.cancel("tab"));
    }

    #[test]
    fn an_ended_job_cannot_be_cancelled() {
        let jobs = Jobs::default();
        let (id, cancel) = jobs.start("tab", "c1");
        assert!(!cancel.is_cancelled());
        jobs.end("tab", id);
        assert!(!jobs.cancel("tab"));
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn ids_increase_across_keys() {
        let jobs = Jobs::default();
        let (first, _) = jobs.start("c1", "conn");
        let (second, _) = jobs.start("c1", "conn");
        let (third, _) = jobs.start("c2", "conn");
        assert!(first < second && second < third);
    }

    #[test]
    fn keys_are_independent_and_connections_cancel_together() {
        let jobs = Jobs::default();
        let (_, a) = jobs.start("tab-a", "c1");
        let (_, b) = jobs.start("import-1", "c1");
        let (_, c) = jobs.start("tab-c", "c2");
        assert!(!a.is_cancelled() && !b.is_cancelled());
        jobs.cancel_connection("c1");
        assert!(a.is_cancelled() && b.is_cancelled() && !c.is_cancelled());
    }
}
