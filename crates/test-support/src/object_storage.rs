//! In-memory [`ObjectStorage`] test double.
//!
//! Used by the web/infrastructure test suite so media flows (upload → gallery)
//! run without a filesystem or a real S3/RustFS, storing bytes in a `HashMap`.
//! `presigned_get` returns an absolute `http://media.test.invalid/...`-shaped
//! URL — the same shape a real presigned S3/RustFS URL has (a full, foreign
//! origin the app never proxies) — purely as a stable, inspectable string for
//! gallery-link assertions; nothing actually serves that origin.
//!
//! [`bikesnest_infrastructure::TEST_MEDIA_ORIGIN`] is the single source of
//! truth for that origin: `Config::for_tests` puts the same string in
//! `security.media_hosts`, so a rendered `<img src>` here and the CSP's
//! `img-src` allowlist are asserting on the same host by construction (see
//! `crates/web/tests/csp_test.rs`).

use async_trait::async_trait;
use bikesnest_application::{ObjectInfo, ObjectPage, ObjectStorage, PutObject, StorageError};
use bikesnest_infrastructure::TEST_MEDIA_ORIGIN;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One stored object: bytes, content type, and the `last_modified` the
/// [`ObjectStorage::list`] walk reports. Tests set it explicitly through
/// [`TestObjectStorage::seed_aged`] to exercise the orphan age gate.
struct Stored {
    bytes: Vec<u8>,
    last_modified: DateTime<Utc>,
}

#[derive(Default)]
pub struct TestObjectStorage {
    store: Mutex<HashMap<String, Stored>>,
    /// When set, every `list` call fails with this error — the "storage is
    /// unreachable" case the retention sweep must propagate rather than
    /// reporting a successful zero.
    list_fails: Mutex<bool>,
    /// Keys per `list` page (default: everything in one page).
    page_size: Mutex<usize>,
}

impl TestObjectStorage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-seed an object so a page/GET can render it without a prior upload.
    /// `content_type` is accepted for parity with a real store but never read
    /// back: nothing in the suite serves these bytes over HTTP.
    pub fn seed(&self, key: &str, bytes: &[u8], _content_type: impl Into<String>) {
        self.store.lock().unwrap().insert(
            key.to_string(),
            Stored {
                bytes: bytes.to_vec(),
                last_modified: Utc::now(),
            },
        );
    }

    /// Pre-seed an object with an explicit `last_modified` (the retention
    /// orphan sweep gates on age before it ever touches the database).
    pub fn seed_aged(&self, key: &str, last_modified: DateTime<Utc>) {
        self.store.lock().unwrap().insert(
            key.to_string(),
            Stored {
                bytes: Vec::new(),
                last_modified,
            },
        );
    }

    /// Make every subsequent `list` fail (simulates an unreachable store).
    pub fn fail_list(&self) {
        *self.list_fails.lock().unwrap() = true;
    }

    /// Return at most `n` keys per `list` page, so a test can drive the
    /// sweep's pagination loop.
    pub fn set_page_size(&self, n: usize) {
        *self.page_size.lock().unwrap() = n;
    }

    pub fn len(&self) -> usize {
        self.store.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, key: &str) -> bool {
        self.store.lock().unwrap().contains_key(key)
    }

    /// Reads a stored object's bytes directly (there is no `/media` route to
    /// fetch them through — this is the test-only substitute for that).
    pub fn get_bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.store.lock().unwrap().get(key).map(|s| s.bytes.clone())
    }
}

#[async_trait]
impl ObjectStorage for TestObjectStorage {
    async fn put(&self, req: PutObject<'_>) -> Result<String, StorageError> {
        self.store.lock().unwrap().insert(
            req.key.clone(),
            Stored {
                bytes: req.bytes.to_vec(),
                last_modified: Utc::now(),
            },
        );
        Ok(req.key)
    }

    async fn presigned_get(&self, key: &str, ttl: Duration) -> Result<String, StorageError> {
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + ttl.as_secs().max(1);
        // Signature is a sentinel; `verify_get` always accepts (test-only store).
        Ok(format!("{TEST_MEDIA_ORIGIN}/{key}?exp={exp}&sig=testsig"))
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.store.lock().unwrap().remove(key);
        Ok(())
    }

    async fn exists(&self, key: &str) -> Result<bool, StorageError> {
        Ok(self.contains(key))
    }

    async fn list(&self, prefix: &str, after: Option<&str>) -> Result<ObjectPage, StorageError> {
        if *self.list_fails.lock().unwrap() {
            return Err(StorageError::Unavailable);
        }
        let mut keys: Vec<ObjectInfo> = self
            .store
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(prefix) && after.is_none_or(|a| k.as_str() > a))
            .map(|(k, v)| ObjectInfo {
                key: k.clone(),
                last_modified: v.last_modified,
            })
            .collect();
        keys.sort_by(|a, b| a.key.cmp(&b.key));

        let page_size = *self.page_size.lock().unwrap();
        if page_size == 0 || keys.len() <= page_size {
            return Ok(ObjectPage {
                objects: keys,
                next: None,
            });
        }
        keys.truncate(page_size);
        let next = keys.last().map(|o| o.key.clone());
        Ok(ObjectPage {
            objects: keys,
            next,
        })
    }
}
