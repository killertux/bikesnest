//! argon2id password hasher. OWASP memory-hard interactive parameters,
//! per-hash random salt encoded in the PHC string, constant-time verify.
//!
//! Both operations are deliberately expensive (m=19 MiB, t=2) and entirely
//! CPU-bound. One shared, bounded admission budget covers hash and verify, and
//! admitted work runs on `tokio::task::spawn_blocking` rather than on a
//! runtime worker thread. Hashing inline would park a worker for tens of
//! milliseconds per call — a burst of logins (each of which hashes twice, once
//! for real and once to equalise timing on an unknown address) would otherwise
//! stall every other request on the runtime, `/healthz` included.

use crate::config::PasswordHashConfig;
use crate::cpu::CpuAdmission;
use argon2::password_hash::PasswordHasher as ArgonPasswordHasher;
use argon2::{
    Argon2, PasswordHash,
    password_hash::{PasswordVerifier, SaltString, rand_core::OsRng},
};
use async_trait::async_trait;
use bikesnest_application::{AuthError, PasswordHasher};
use bikesnest_domain::Password;

/// Implements [`PasswordHasher`] with argon2id default params
/// (m=19456 KiB, t=2, p=1 — OWASP interactive login baseline).
#[derive(Clone)]
pub struct Argon2PasswordHasher {
    admission: CpuAdmission,
    #[cfg(test)]
    blocking_hook: Option<std::sync::Arc<dyn Fn() -> Box<dyn Send> + Send + Sync>>,
}

impl std::fmt::Debug for Argon2PasswordHasher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Argon2PasswordHasher")
            .finish_non_exhaustive()
    }
}

impl Argon2PasswordHasher {
    /// Creates one process budget. The composition root constructs this once;
    /// every service receives a clone sharing the same semaphores.
    pub fn new(config: PasswordHashConfig) -> Self {
        Self {
            admission: CpuAdmission::new(
                config.concurrency,
                config.queue_capacity,
                config.admission_timeout,
            ),
            #[cfg(test)]
            blocking_hook: None,
        }
    }

    #[cfg(test)]
    fn with_hook(
        config: PasswordHashConfig,
        hook: std::sync::Arc<dyn Fn() -> Box<dyn Send> + Send + Sync>,
    ) -> Self {
        let mut hasher = Self::new(config);
        hasher.blocking_hook = Some(hook);
        hasher
    }

    #[cfg(test)]
    fn waiting_available(&self) -> usize {
        self.admission.waiting_available()
    }
}

#[async_trait]
impl PasswordHasher for Argon2PasswordHasher {
    async fn hash(&self, pw: &Password) -> Result<String, AuthError> {
        // The secret is copied into the blocking task; the borrow cannot cross
        // the spawn boundary.
        let secret = pw.as_str().to_string();
        #[cfg(test)]
        let hook = self.blocking_hook.clone();
        self.admission
            .run(move || {
                #[cfg(test)]
                let _work_guard = hook.map(|hook| hook());
                let salt = SaltString::generate(&mut OsRng);
                Argon2::default()
                    .hash_password(secret.as_bytes(), &salt)
                    .map(|h| h.to_string())
                    .map_err(|_| AuthError::Internal)
            })
            .await
            .map_err(|_| AuthError::Internal)?
    }

    async fn verify(&self, pw: &Password, hash: &str) -> Result<bool, AuthError> {
        let secret = pw.as_str().to_string();
        let hash = hash.to_string();
        #[cfg(test)]
        let hook = self.blocking_hook.clone();
        self.admission
            .run(move || {
                #[cfg(test)]
                let _work_guard = hook.map(|hook| hook());
                let Ok(parsed) = PasswordHash::new(&hash) else {
                    return false;
                };
                Argon2::default()
                    .verify_password(secret.as_bytes(), &parsed)
                    .is_ok()
            })
            .await
            .map_err(|_| AuthError::Internal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    struct BlockingGate {
        entered: AtomicUsize,
        active: AtomicUsize,
        peak: AtomicUsize,
        released: Mutex<bool>,
        wake: Condvar,
    }

    struct ActiveGuard(Arc<BlockingGate>);
    impl Drop for ActiveGuard {
        fn drop(&mut self) {
            self.0.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct ReleaseOnDrop(Arc<BlockingGate>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    impl BlockingGate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                entered: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                released: Mutex::new(false),
                wake: Condvar::new(),
            })
        }

        fn enter(self: &Arc<Self>) -> Box<dyn Send> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            let mut released = self.released.lock().expect("gate lock");
            while !*released {
                released = self.wake.wait(released).expect("gate wait");
            }
            drop(released);
            Box::new(ActiveGuard(self.clone()))
        }

        fn release(&self) {
            *self.released.lock().expect("gate lock") = true;
            self.wake.notify_all();
        }
    }

    async fn wait_until(predicate: impl Fn() -> bool) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !predicate() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("condition timed out");
    }

    /// The offload must not need a second runtime thread: `spawn_blocking`
    /// dispatches to the blocking pool, so hashing completes even on a
    /// single-threaded runtime (a naive `block_in_place` would deadlock here).
    #[tokio::test(flavor = "current_thread")]
    async fn hash_and_verify_complete_on_a_single_threaded_runtime() {
        let hasher = Argon2PasswordHasher::new(PasswordHashConfig::default());
        let pw = Password::new("correct horse battery staple");
        let hash = hasher.hash(&pw).await.expect("hashing succeeds");
        assert!(hash.starts_with("$argon2id$"), "PHC string: {hash}");
        assert!(hasher.verify(&pw, &hash).await.expect("verify runs"));
        assert!(
            !hasher
                .verify(&Password::new("wrong"), &hash)
                .await
                .expect("verify runs")
        );
        // A malformed stored hash is a `false`, never an error.
        assert!(!hasher.verify(&pw, "not-a-phc-string").await.unwrap());
    }

    #[tokio::test]
    async fn cancelled_running_hash_keeps_capacity_until_blocking_work_exits() {
        let gate = BlockingGate::new();
        let _release = ReleaseOnDrop(gate.clone());
        let hook = {
            let gate = gate.clone();
            Arc::new(move || gate.enter()) as Arc<dyn Fn() -> Box<dyn Send> + Send + Sync>
        };
        let hasher = Argon2PasswordHasher::with_hook(
            PasswordHashConfig {
                concurrency: 1,
                queue_capacity: 1,
                admission_timeout: std::time::Duration::from_millis(40),
            },
            hook,
        );
        let first_hasher = hasher.clone();
        let first =
            tokio::spawn(async move { first_hasher.hash(&Password::new("first password")).await });
        wait_until(|| gate.entered.load(Ordering::SeqCst) == 1).await;
        first.abort();
        let _ = first.await;
        assert_eq!(
            hasher.verify(&Password::new("other password"), "bad").await,
            Err(AuthError::Internal)
        );
        assert_eq!(gate.entered.load(Ordering::SeqCst), 1);
        gate.release();
        wait_until(|| gate.active.load(Ordering::SeqCst) == 0).await;
        let hash = hasher
            .hash(&Password::new("recovery password"))
            .await
            .unwrap();
        assert!(
            hasher
                .verify(&Password::new("recovery password"), &hash)
                .await
                .unwrap()
        );
        assert_eq!(gate.peak.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn waiting_queue_is_bounded_and_cancelled_waiter_releases_admission() {
        let gate = BlockingGate::new();
        let _release = ReleaseOnDrop(gate.clone());
        let hook = {
            let gate = gate.clone();
            Arc::new(move || gate.enter()) as Arc<dyn Fn() -> Box<dyn Send> + Send + Sync>
        };
        let hasher = Argon2PasswordHasher::with_hook(
            PasswordHashConfig {
                concurrency: 1,
                queue_capacity: 1,
                admission_timeout: std::time::Duration::from_millis(200),
            },
            hook,
        );
        let running_hasher = hasher.clone();
        let running = tokio::spawn(async move {
            running_hasher
                .hash(&Password::new("running password"))
                .await
        });
        wait_until(|| gate.entered.load(Ordering::SeqCst) == 1).await;
        let queued_hasher = hasher.clone();
        let queued = tokio::spawn(async move {
            queued_hasher
                .verify(&Password::new("queued password"), "bad")
                .await
        });
        wait_until(|| hasher.waiting_available() == 0).await;
        assert_eq!(
            hasher.hash(&Password::new("queue full password")).await,
            Err(AuthError::Internal)
        );
        queued.abort();
        let _ = queued.await;
        wait_until(|| hasher.waiting_available() == 1).await;
        assert_eq!(
            hasher.verify(&Password::new("times out"), "bad").await,
            Err(AuthError::Internal)
        );
        gate.release();
        running.await.unwrap().unwrap();
        assert_eq!(gate.peak.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn real_hash_burst_never_exceeds_configured_blocking_capacity() {
        let gate = BlockingGate::new();
        let _release = ReleaseOnDrop(gate.clone());
        let hook = {
            let gate = gate.clone();
            Arc::new(move || gate.enter()) as Arc<dyn Fn() -> Box<dyn Send> + Send + Sync>
        };
        let hasher = Argon2PasswordHasher::with_hook(
            PasswordHashConfig {
                concurrency: 2,
                queue_capacity: 6,
                admission_timeout: std::time::Duration::from_secs(20),
            },
            hook,
        );
        let started = std::time::Instant::now();
        let tasks: Vec<_> = (0..6)
            .map(|index| {
                let hasher = hasher.clone();
                tokio::spawn(async move {
                    let secret = format!("burst password {index}");
                    hasher.hash(&Password::new(&secret)).await
                })
            })
            .collect();
        wait_until(|| gate.entered.load(Ordering::SeqCst) == 2).await;
        assert_eq!(gate.peak.load(Ordering::SeqCst), 2);
        gate.release();
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        assert_eq!(gate.peak.load(Ordering::SeqCst), 2);
        eprintln!(
            "argon2 debug burst six elapsed_ms={}",
            started.elapsed().as_millis()
        );
    }
}
