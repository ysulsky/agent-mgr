//! `asyncio.Event` for tokio: a flag that stays set until cleared, and wakes every waiter.
//! The daemon's loops depend on its exact semantics: a loop clears its event before it
//! works, so a set during the work brings on another round at once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Default)]
pub struct Event {
    flag: AtomicBool,
    notify: Notify,
}

impl Event {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn clear(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }

    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    pub async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_set() {
                return;
            }
            notified.await;
        }
    }

    /// `asyncio.wait_for(event.wait(), seconds)` with the timeout suppressed: true if it
    /// was set.
    pub async fn wait_for(&self, seconds: f64) -> bool {
        let seconds = if seconds.is_finite() && seconds > 0.0 { seconds } else { 0.0 };
        tokio::time::timeout(Duration::from_secs_f64(seconds), self.wait()).await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn set_wakes_waiters_and_stays_set_until_cleared() {
        let event = Arc::new(Event::new());
        assert!(!event.wait_for(0.01).await);
        let other = event.clone();
        let waiter = tokio::spawn(async move { other.wait_for(5.0).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        event.set();
        assert!(waiter.await.unwrap());
        assert!(event.wait_for(0.0).await, "still set");
        event.clear();
        assert!(!event.wait_for(0.01).await);
    }
}
