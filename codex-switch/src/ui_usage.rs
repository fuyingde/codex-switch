//! Bounded background usage queries. No windows, credentials or global env changes.
use super::{usage, Message, Request};
use std::sync::{mpsc, Arc, Condvar, Mutex};

pub(super) const DEFAULT_CONCURRENCY: u32 = 5;
pub(super) const MAX_CONCURRENCY: u32 = 16;
type Read = dyn Fn(&Request) -> Result<usage::Usage, String> + Send + Sync;

struct Limit {
    value: usize,
    active: usize,
}

pub(super) struct Controller {
    limit: Arc<(Mutex<Limit>, Condvar)>,
    queue: Arc<Mutex<mpsc::Receiver<Request>>>,
    read: Arc<Read>,
    notify: Arc<dyn Fn() + Send + Sync>,
    results: mpsc::Sender<Message>,
    workers: usize,
}

impl Controller {
    pub(super) fn set_limit(&mut self, value: u32) {
        let value = value.clamp(1, MAX_CONCURRENCY) as usize;
        self.limit.0.lock().unwrap().value = value;
        self.limit.1.notify_all();
        // Grow only when requested, rather than allocating sixteen workers at startup.
        // Existing workers wait when the limit decreases; running reads finish normally.
        while self.workers < value {
            let queue = Arc::clone(&self.queue);
            let limit = Arc::clone(&self.limit);
            let read = Arc::clone(&self.read);
            let notify = Arc::clone(&self.notify);
            let results = self.results.clone();
            std::thread::spawn(move || loop {
                let request = {
                    let Ok(receiver) = queue.lock() else { break };
                    receiver.recv()
                };
                let Ok(request) = request else { break };
                let (lock, changed) = &*limit;
                let mut state = lock.lock().unwrap();
                while state.active >= state.value {
                    state = changed.wait(state).unwrap();
                }
                state.active += 1;
                drop(state);
                let result = read(&request);
                lock.lock().unwrap().active -= 1;
                changed.notify_all();
                if results
                    .send(Message::Usage {
                        id: request.id,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
                notify();
            });
            self.workers += 1;
        }
    }
}

pub(super) fn spawn(
    concurrency: u32,
    read: impl Fn(&Request) -> Result<usage::Usage, String> + Send + Sync + 'static,
    notify: impl Fn() + Send + Sync + 'static,
) -> (mpsc::Sender<Request>, mpsc::Receiver<Message>, Controller) {
    let (tx_request, rx_request) = mpsc::channel::<Request>();
    let (tx_message, rx_message) = mpsc::channel::<Message>();
    let mut controller = Controller {
        limit: Arc::new((
            Mutex::new(Limit {
                value: 1,
                active: 0,
            }),
            Condvar::new(),
        )),
        queue: Arc::new(Mutex::new(rx_request)),
        read: Arc::new(read),
        notify: Arc::new(notify),
        results: tx_message,
        workers: 0,
    };
    controller.set_limit(concurrency);
    (tx_request, rx_message, controller)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn queries_respect_configured_limits_and_errors_do_not_stop_the_queue() {
        for concurrency in [1, DEFAULT_CONCURRENCY, MAX_CONCURRENCY] {
            check_limit(concurrency);
        }
    }

    fn check_limit(concurrency: u32) {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, starts) = mpsc::channel();
        let (tx, rx, _control) = spawn(
            concurrency,
            {
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                let gate = Arc::clone(&gate);
                move |request| {
                    assert_eq!(
                        request.home,
                        PathBuf::from(format!("isolated-{}", request.id))
                    );
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    started.send(request.id.clone()).unwrap();
                    let (lock, changed) = &*gate;
                    let (released, timeout) = changed
                        .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |open| {
                            !*open
                        })
                        .unwrap();
                    let ok = *released && !timeout.timed_out();
                    active.fetch_sub(1, Ordering::SeqCst);
                    if !ok {
                        return Err("synthetic test gate timed out".into());
                    }
                    if request.id == "failure" {
                        Err("synthetic per-account failure".into())
                    } else {
                        Ok(usage::Usage::default())
                    }
                }
            },
            || {},
        );
        let ids: Vec<_> = (0..concurrency + 4)
            .map(|i| {
                if i == 2 {
                    "failure".into()
                } else {
                    format!("account-{i}")
                }
            })
            .collect();
        for id in &ids {
            tx.send(Request {
                id: id.clone(),
                home: PathBuf::from(format!("isolated-{id}")),
            })
            .unwrap();
        }
        // Every configured slot starts before ANY query is released.
        let first: HashSet<_> = (0..concurrency as usize)
            .map(|_| starts.recv_timeout(Duration::from_secs(3)).unwrap())
            .collect();
        assert_eq!(first.len(), concurrency as usize);
        assert_eq!(active.load(Ordering::SeqCst), concurrency as usize);
        assert!(
            starts.try_recv().is_err(),
            "the next query must wait for a slot"
        );
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let mut received = HashSet::new();
        for _ in &ids {
            let Message::Usage { id, result } = rx.recv_timeout(Duration::from_secs(3)).unwrap();
            if id == "failure" {
                assert_eq!(result.unwrap_err(), "synthetic per-account failure");
            } else {
                assert!(result.is_ok());
            }
            assert!(received.insert(id));
        }
        assert_eq!(received.len(), ids.len());
        assert_eq!(peak.load(Ordering::SeqCst), concurrency as usize);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        drop(tx);
    }

    #[test]
    fn a_slow_first_account_does_not_hold_back_later_results() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (tx, rx, _control) = spawn(
            DEFAULT_CONCURRENCY,
            {
                let gate = Arc::clone(&gate);
                move |request| {
                    if request.id == "slow" {
                        let (lock, changed) = &*gate;
                        let (_released, timeout) = changed
                            .wait_timeout_while(
                                lock.lock().unwrap(),
                                Duration::from_secs(5),
                                |open| !*open,
                            )
                            .unwrap();
                        if timeout.timed_out() {
                            return Err("synthetic slow query timed out".into());
                        }
                    }
                    Ok(usage::Usage::default())
                }
            },
            || {},
        );
        for id in ["slow", "fast"] {
            tx.send(Request {
                id: id.into(),
                home: PathBuf::from(id),
            })
            .unwrap();
        }
        let Message::Usage { id, result } = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(id, "fast");
        assert!(result.is_ok());
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let Message::Usage { id, result } = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(id, "slow");
        assert!(result.is_ok());
        drop(tx);
    }
    #[test]
    fn saved_limit_changes_apply_to_queued_queries_without_interrupting_running_reads() {
        let gate = Arc::new((Mutex::new(HashSet::<String>::new()), Condvar::new()));
        let (started, starts) = mpsc::channel();
        let (tx, rx, mut control) = spawn(
            1,
            {
                let gate = Arc::clone(&gate);
                move |request| {
                    started.send(request.id.clone()).unwrap();
                    let (lock, changed) = &*gate;
                    let (_released, timeout) = changed
                        .wait_timeout_while(
                            lock.lock().unwrap(),
                            Duration::from_secs(5),
                            |released| !released.contains(&request.id),
                        )
                        .unwrap();
                    if timeout.timed_out() {
                        return Err("synthetic query timed out".into());
                    }
                    Ok(usage::Usage::default())
                }
            },
            || {},
        );
        for i in 0..8 {
            tx.send(Request {
                id: i.to_string(),
                home: PathBuf::from("isolated"),
            })
            .unwrap();
        }
        let mut running = vec![starts.recv_timeout(Duration::from_secs(3)).unwrap()];
        assert!(starts.recv_timeout(Duration::from_millis(60)).is_err());
        control.set_limit(3);
        for _ in 0..2 {
            running.push(starts.recv_timeout(Duration::from_secs(3)).unwrap());
        }
        control.set_limit(1);
        // Reducing the limit does not interrupt any of the three reads.
        assert!(rx.try_recv().is_err());
        for id in &running[..2] {
            gate.0.lock().unwrap().insert(id.clone());
        }
        gate.1.notify_all();
        for _ in 0..2 {
            let Message::Usage { result, .. } = rx.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(result.is_ok());
        }
        assert!(
            starts.recv_timeout(Duration::from_millis(60)).is_err(),
            "new queries must wait until active reads fall below the reduced limit"
        );
        gate.0.lock().unwrap().insert(running[2].clone());
        gate.1.notify_all();
        let Message::Usage { result, .. } = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(result.is_ok());
        let next = starts.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(!running.contains(&next));
        assert!(starts.recv_timeout(Duration::from_millis(60)).is_err());
        gate.0.lock().unwrap().extend((0..8).map(|i| i.to_string()));
        gate.1.notify_all();
        for _ in 0..5 {
            let Message::Usage { result, .. } = rx.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(result.is_ok());
        }
    }
}
