use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

type ServerCancel = Box<dyn Fn() + Send + Sync>;

#[derive(Clone, Default)]
pub struct Cancel(Arc<CancelState>);

#[derive(Default)]
struct CancelState {
    cancelled: AtomicBool,
    notify: Notify,
    next_id: AtomicU64,
    server: Mutex<Option<(u64, ServerCancel)>>,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if self.0.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        self.0.notify.notify_waiters();
        if let Some((_, server)) = self.0.server.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            server();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    // A cancel that arrives before the statement starts still reaches the server.
    pub fn on_server(&self, server: impl Fn() + Send + Sync + 'static) -> ServerCancelGuard {
        let id = self.0.next_id.fetch_add(1, Ordering::SeqCst);
        *self.0.server.lock().unwrap_or_else(|e| e.into_inner()) = Some((id, Box::new(server)));
        if self.is_cancelled()
            && let Some((_, server)) = self.0.server.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        {
            server();
        }
        ServerCancelGuard { cancel: self.clone(), id }
    }
}

pub struct ServerCancelGuard {
    cancel: Cancel,
    id: u64,
}

impl Drop for ServerCancelGuard {
    fn drop(&mut self) {
        let mut server = self.cancel.0.server.lock().unwrap_or_else(|e| e.into_inner());
        if server.as_ref().is_some_and(|(id, _)| *id == self.id) {
            *server = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn cancel_wakes_waiters_and_reaches_the_server_once() {
        let cancel = Cancel::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let guard = cancel.on_server(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let waiter = tokio::spawn({
            let cancel = cancel.clone();
            async move { cancel.cancelled().await }
        });
        cancel.cancel();
        cancel.cancel();
        waiter.await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        drop(guard);
        assert!(cancel.0.server.lock().unwrap().is_none());
    }

    #[test]
    fn a_late_registration_fires_immediately() {
        let cancel = Cancel::new();
        cancel.cancel();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let _guard = cancel.on_server(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
