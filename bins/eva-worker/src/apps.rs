//! The apps installed on this Mac, as the command parser sees them.
//!
//! Scanned when the worker starts, and again when a command names an app the
//! index does not know — so an app installed after EVA01 started (the very
//! thing "abre Photoshop → no está instalada → App Store" leads to) works the
//! next time it is asked for, without restarting anything.

use eva_intent::AppIndex;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// Two rescans are never closer than this: a run of misheard names must not
/// walk the disk each time.
const MIN_RESCAN_GAP: Duration = Duration::from_secs(5);

type Scan = Box<dyn Fn() -> AppIndex + Send + Sync>;

/// The current index of installed apps, refreshable.
pub struct AppCatalog {
    index: RwLock<Arc<AppIndex>>,
    scan: Option<Scan>,
    last_scan: Mutex<Instant>,
    /// How the user says their apps (`heard`, canonical name), confirmed
    /// once; applied on top of every scan.
    learned: Mutex<Vec<(String, String)>>,
}

impl AppCatalog {
    /// A catalog filled by `scan` now, and by `scan` again on [`AppCatalog::refresh`].
    pub fn new(scan: impl Fn() -> AppIndex + Send + Sync + 'static) -> AppCatalog {
        let index = scan();
        AppCatalog {
            index: RwLock::new(Arc::new(index)),
            scan: Some(Box::new(scan)),
            last_scan: Mutex::new(Instant::now()),
            learned: Mutex::new(Vec::new()),
        }
    }

    /// Starts out knowing how the user says their apps.
    #[must_use]
    pub fn with_learned(self, learned: Vec<(String, String)>) -> AppCatalog {
        for (heard, app) in learned {
            self.learn(&heard, &app);
        }
        self
    }

    /// From now on `heard` is an exact name of `app`, through rescans too.
    pub fn learn(&self, heard: &str, app: &str) {
        #[allow(clippy::unwrap_used)] // only poisoned if a holder panicked, forbidden by workspace policy
        self.learned.lock().unwrap().push((heard.to_string(), app.to_string()));
        let mut index = (*self.current()).clone();
        index.teach(heard, app);
        #[allow(clippy::unwrap_used)] // as above
        {
            *self.index.write().unwrap() = Arc::new(index);
        }
    }

    /// A catalog that is exactly `index` and never rescans (tests).
    #[cfg(test)]
    pub fn fixed(index: AppIndex) -> AppCatalog {
        AppCatalog {
            index: RwLock::new(Arc::new(index)),
            scan: None,
            last_scan: Mutex::new(Instant::now()),
            learned: Mutex::new(Vec::new()),
        }
    }

    /// Lets the next [`AppCatalog::refresh`] scan at once (tests).
    #[cfg(test)]
    pub fn allow_rescan_now(&self) {
        #[allow(clippy::unwrap_used)] // only poisoned if a holder panicked, forbidden by workspace policy
        {
            *self.last_scan.lock().unwrap() = Instant::now() - MIN_RESCAN_GAP - Duration::from_secs(1);
        }
    }

    /// The index as of the last scan.
    pub fn current(&self) -> Arc<AppIndex> {
        #[allow(clippy::unwrap_used)] // only poisoned if a writer panicked, forbidden by workspace policy
        Arc::clone(&self.index.read().unwrap())
    }

    /// Scans again, unless it just did. `true` if the index was replaced.
    pub fn refresh(&self) -> bool {
        let Some(scan) = &self.scan else { return false };
        #[allow(clippy::unwrap_used)] // only poisoned if a holder panicked, forbidden by workspace policy
        let mut last = self.last_scan.lock().unwrap();
        if last.elapsed() < MIN_RESCAN_GAP {
            return false;
        }
        *last = Instant::now();
        drop(last);
        let mut fresh = scan();
        #[allow(clippy::unwrap_used)] // as above
        for (heard, app) in self.learned.lock().unwrap().iter() {
            fresh.teach(heard, app);
        }
        let fresh = Arc::new(fresh);
        #[allow(clippy::unwrap_used)] // as above
        {
            *self.index.write().unwrap() = fresh;
        }
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use eva_intent::AppEntry;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counting_catalog() -> (AppCatalog, Arc<AtomicUsize>) {
        let scans = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&scans);
        let catalog = AppCatalog::new(move || {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            AppIndex::new(vec![AppEntry::new(format!("App{n}"))])
        });
        (catalog, scans)
    }

    #[test]
    fn a_fresh_catalog_has_scanned_once_and_does_not_rescan_at_once() {
        let (catalog, scans) = counting_catalog();
        assert_eq!(scans.load(Ordering::SeqCst), 1);
        assert!(!catalog.refresh(), "it scanned a moment ago");
        assert_eq!(scans.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn after_the_gap_a_refresh_replaces_the_index() {
        let (catalog, scans) = counting_catalog();
        *catalog.last_scan.lock().unwrap() = Instant::now() - MIN_RESCAN_GAP - Duration::from_secs(1);

        assert!(catalog.refresh());

        assert_eq!(scans.load(Ordering::SeqCst), 2);
        assert!(catalog.current().find("App1").is_some(), "the new scan is what is served");
        assert!(!catalog.refresh(), "and the gap starts over");
    }

    #[test]
    fn a_fixed_catalog_never_rescans() {
        assert!(!AppCatalog::fixed(AppIndex::new(Vec::new())).refresh());
    }
}
