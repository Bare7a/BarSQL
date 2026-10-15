use std::sync::Arc;
use std::time::Duration;

use barsql_core::QueryError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

// Open tabs keep their connections until idle for ten minutes, so waiting longer than this seldom helps.
pub(crate) const LEASE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) async fn acquire(
    permits: &Arc<Semaphore>,
    size: usize,
    wait: Duration,
) -> Result<OwnedSemaphorePermit, QueryError> {
    match tokio::time::timeout(wait, permits.clone().acquire_owned()).await {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) => Err(QueryError::message("connection is closed")),
        Err(_) => Err(QueryError {
            message: format!("connection pool is busy: all {size} connections are in use"),
            hint: "Each open query tab keeps a connection. Close a tab to free one.".into(),
            ..Default::default()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_full_pool_fails_after_the_wait() {
        let permits = Arc::new(Semaphore::new(2));
        let wait = Duration::from_millis(20);
        let held = [acquire(&permits, 2, wait).await.unwrap(), acquire(&permits, 2, wait).await.unwrap()];
        let error = acquire(&permits, 2, wait).await.unwrap_err();
        assert_eq!(error.message, "connection pool is busy: all 2 connections are in use");
        drop(held);
        drop(acquire(&permits, 2, wait).await.unwrap());
        permits.close();
        assert_eq!(acquire(&permits, 2, wait).await.unwrap_err().message, "connection is closed");
    }
}
