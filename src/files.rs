//! Watched files are checked at most once per [`WATCH_INTERVAL`], so a new
//! hot file or build is picked up within a second without a filesystem call
//! on every render; otherwise their first result is cached for the process
//! lifetime.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

/// How long a watched file's answer is reused before it's checked again.
const WATCH_INTERVAL: Duration = Duration::from_secs(1);

type Cache<T> = LazyLock<RwLock<HashMap<PathBuf, Entry<T>>>>;

static CONTENTS: Cache<Option<Arc<str>>> = LazyLock::new(RwLock::default);
static EXISTS: Cache<bool> = LazyLock::new(RwLock::default);

struct Entry<T> {
    value: T,
    checked: Instant,
}

/// The file's contents, or `None` when it can't be read.
pub(crate) fn read(path: &Path, watch: bool) -> Option<Arc<str>> {
    read_within(path, max_age(watch))
}

/// Whether the file exists.
pub(crate) fn exists(path: &Path, watch: bool) -> bool {
    exists_within(path, max_age(watch))
}

fn max_age(watch: bool) -> Option<Duration> {
    watch.then_some(WATCH_INTERVAL)
}

fn read_within(path: &Path, max_age: Option<Duration>) -> Option<Arc<str>> {
    remember(&CONTENTS, path, max_age, || {
        std::fs::read_to_string(path).ok().map(Arc::from)
    })
}

fn exists_within(path: &Path, max_age: Option<Duration>) -> bool {
    remember(&EXISTS, path, max_age, || path.exists())
}

/// The remembered answer for `path`, loaded again once it's older than
/// `max_age`. Without a `max_age`, the first answer is kept forever.
fn remember<T: Clone>(
    cache: &RwLock<HashMap<PathBuf, Entry<T>>>,
    path: &Path,
    max_age: Option<Duration>,
    load: impl FnOnce() -> T,
) -> T {
    let is_fresh = |entry: &Entry<T>| max_age.is_none_or(|max_age| entry.checked.elapsed() < max_age);

    if let Some(entry) = cache.read().unwrap_or_else(PoisonError::into_inner).get(path)
        && is_fresh(entry)
    {
        return entry.value.clone();
    }

    let value = load();
    let mut cache = cache.write().unwrap_or_else(PoisonError::into_inner);

    // Another thread may have loaded it in the meantime. Unwatched, the
    // first answer is kept for good, so every caller sees the same one.
    if max_age.is_none()
        && let Some(entry) = cache.get(path)
    {
        return entry.value.clone();
    }

    cache.insert(
        path.to_owned(),
        Entry {
            value: value.clone(),
            checked: Instant::now(),
        },
    );
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path unique per test, since answers are kept for the whole process.
    fn path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("inertia-files-{name}-{}", std::process::id()))
    }

    #[test]
    fn watched_files_are_checked_again_once_stale() {
        let path = path("watched");
        assert_eq!(read_within(&path, Some(Duration::ZERO)), None);
        assert!(!exists_within(&path, Some(Duration::ZERO)));

        std::fs::write(&path, "http://localhost:5173").unwrap();

        assert_eq!(
            read_within(&path, Some(Duration::ZERO)).as_deref(),
            Some("http://localhost:5173")
        );
        assert!(exists_within(&path, Some(Duration::ZERO)));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn watched_files_are_not_checked_again_while_fresh() {
        let path = path("fresh");
        assert_eq!(read_within(&path, Some(Duration::from_secs(60))), None);
        assert!(!exists_within(&path, Some(Duration::from_secs(60))));

        std::fs::write(&path, "http://localhost:5173").unwrap();

        assert_eq!(read_within(&path, Some(Duration::from_secs(60))), None);
        assert!(!exists_within(&path, Some(Duration::from_secs(60))));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn unwatched_files_keep_their_first_answer() {
        let (missing, present) = (path("missing"), path("present"));
        std::fs::write(&present, "http://localhost:5173").unwrap();
        assert_eq!(read(&missing, false), None);
        assert!(!exists(&missing, false));
        assert!(exists(&present, false));

        std::fs::write(&missing, "http://localhost:5173").unwrap();
        std::fs::remove_file(&present).unwrap();

        assert_eq!(read(&missing, false), None);
        assert!(!exists(&missing, false));
        assert!(exists(&present, false));
        std::fs::remove_file(missing).unwrap();
    }
}
