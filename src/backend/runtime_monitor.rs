//! Event-driven runtime refresh: kernel link/address/route notifications and
//! a single one-shot failback timer trigger a fresh snapshot of the stored,
//! already authorized policy. Notifications are never trusted as commands.

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::RuntimeStatus;

const IP_BINARY: &str = "/usr/sbin/ip";
#[derive(Debug, Clone, Copy)]
pub struct MonitorTiming {
    /// A burst of notifications (including the ones RouteLane's own route
    /// writes cause) produces one refresh after this window.
    pub coalesce: Duration,
    /// Bounded wait before restarting a monitor that closed or failed.
    pub restart_backoff: Duration,
}

impl Default for MonitorTiming {
    fn default() -> Self {
        Self {
            coalesce: Duration::from_millis(150),
            restart_backoff: Duration::from_secs(1),
        }
    }
}

/// What the monitor can ask the privileged runtime to do.
pub trait RuntimeRefresher: Send + Sync + 'static {
    fn refresh(&self) -> Pin<Box<dyn Future<Output = Option<RuntimeStatus>> + Send + '_>>;
    fn degrade(&self, reason: String) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorEvent {
    /// Something changed; the content is irrelevant.
    Changed,
    /// The notification source ended; a restart and resnapshot follow.
    Closed(String),
}

/// Runs until `events` closes. Subscribing happens before the initial
/// refresh, so a change between them is never missed.
pub async fn drive_runtime(
    refresher: &impl RuntimeRefresher,
    mut events: mpsc::Receiver<MonitorEvent>,
    timing: MonitorTiming,
    restart: impl Fn(),
) {
    let mut deadline = failback_wait(refresher.refresh().await);
    loop {
        let event = match deadline {
            Some(wait) => match tokio::time::timeout(wait, events.recv()).await {
                Ok(event) => event,
                Err(_) => {
                    deadline = failback_wait(refresher.refresh().await);
                    continue;
                }
            },
            None => events.recv().await,
        };
        match event {
            None => return,
            Some(MonitorEvent::Changed) => {
                tokio::time::sleep(timing.coalesce).await;
                while let Ok(MonitorEvent::Changed) = events.try_recv() {}
                deadline = failback_wait(refresher.refresh().await);
            }
            Some(MonitorEvent::Closed(reason)) => {
                refresher.degrade(reason).await;
                tokio::time::sleep(timing.restart_backoff).await;
                restart();
                deadline = failback_wait(refresher.refresh().await);
            }
        }
    }
}

fn failback_wait(status: Option<RuntimeStatus>) -> Option<Duration> {
    status
        .and_then(|status| status.failback_in_ms)
        .map(Duration::from_millis)
}

/// Forwards `ip monitor link address route` notifications into a bounded
/// channel (capacity 1: a pending "changed" already covers later ones).
pub async fn forward_kernel_notifications(events: mpsc::Sender<MonitorEvent>) {
    let child = Command::new(IP_BINARY)
        .args(["monitor", "link", "address", "route"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            let _ = events
                .send(MonitorEvent::Closed(format!(
                    "start kernel monitor: {error}"
                )))
                .await;
            return;
        }
    };
    let stdout = child.stdout.take().expect("piped monitor stdout");
    let mut lines = BufReader::new(stdout).lines();
    let reason = loop {
        match lines.next_line().await {
            Ok(Some(_)) => {
                if let Err(mpsc::error::TrySendError::Closed(_)) =
                    events.try_send(MonitorEvent::Changed)
                {
                    return;
                }
            }
            Ok(None) => break "kernel monitor closed".to_owned(),
            Err(error) => break format!("read kernel monitor: {error}"),
        }
    };
    let _ = child.kill().await;
    let _ = events.send(MonitorEvent::Closed(reason)).await;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Default)]
    struct Counting {
        refreshes: AtomicUsize,
        degraded: Mutex<Vec<String>>,
        failback_in_ms: Mutex<Option<u64>>,
    }

    impl RuntimeRefresher for Counting {
        fn refresh(&self) -> Pin<Box<dyn Future<Output = Option<RuntimeStatus>> + Send + '_>> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            let failback_in_ms = self.failback_in_ms.lock().unwrap().take();
            Box::pin(async move {
                Some(RuntimeStatus {
                    failback_in_ms,
                    ..RuntimeStatus::default()
                })
            })
        }
        fn degrade(&self, reason: String) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.degraded.lock().unwrap().push(reason);
            Box::pin(async {})
        }
    }

    const TIMING: MonitorTiming = MonitorTiming {
        coalesce: Duration::from_millis(20),
        restart_backoff: Duration::from_millis(30),
    };

    fn refreshes(refresher: &Counting) -> usize {
        refresher.refreshes.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn bursts_of_notifications_coalesce_into_one_refresh() {
        let refresher = Arc::new(Counting::default());
        let (sender, receiver) = mpsc::channel(1);
        let driver = {
            let refresher = Arc::clone(&refresher);
            tokio::spawn(async move { drive_runtime(&*refresher, receiver, TIMING, || {}).await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(refreshes(&refresher), 1, "initial snapshot");
        for _ in 0..100 {
            let _ = sender.try_send(MonitorEvent::Changed);
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(refreshes(&refresher), 2);
        drop(sender);
        driver.await.unwrap();
    }

    #[tokio::test]
    async fn failback_deadline_is_a_single_one_shot_timer() {
        let refresher = Arc::new(Counting::default());
        *refresher.failback_in_ms.lock().unwrap() = Some(200);
        let (sender, receiver) = mpsc::channel(1);
        let driver = {
            let refresher = Arc::clone(&refresher);
            tokio::spawn(async move { drive_runtime(&*refresher, receiver, TIMING, || {}).await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(refreshes(&refresher), 1);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(refreshes(&refresher), 2);
        // No further deadline: the driver waits for events instead of polling.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(refreshes(&refresher), 2);
        drop(sender);
        driver.await.unwrap();
    }

    #[tokio::test]
    async fn closed_monitor_degrades_restarts_and_resnapshots() {
        let refresher = Arc::new(Counting::default());
        let restarts = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = mpsc::channel(1);
        let driver = {
            let refresher = Arc::clone(&refresher);
            let restarts = Arc::clone(&restarts);
            tokio::spawn(async move {
                drive_runtime(&*refresher, receiver, TIMING, move || {
                    restarts.fetch_add(1, Ordering::SeqCst);
                })
                .await
            })
        };
        sender
            .send(MonitorEvent::Closed("kernel monitor closed".into()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            *refresher.degraded.lock().unwrap(),
            vec!["kernel monitor closed".to_owned()]
        );
        assert_eq!(restarts.load(Ordering::SeqCst), 1);
        assert_eq!(refreshes(&refresher), 2);
        drop(sender);
        driver.await.unwrap();
    }
}
