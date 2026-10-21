use std::path::PathBuf;
use std::time::SystemTime;

/// Watches the files actually loaded, including imports. Failed edits are logged
/// once and leave the active configuration intact; explicit reload always retries.
pub struct Watcher {
    pub path: Option<PathBuf>,
    sources: Vec<PathBuf>,
    stamps: Vec<Option<(SystemTime, u64)>>,
}
impl Watcher {
    pub fn new(path: Option<PathBuf>) -> Self {
        let sources = path.as_ref().map_or_else(Vec::new, |path| {
            aura_config::AppConfig::load_with_sources(path)
                .map_or_else(|_| vec![path.clone()], |(_, sources)| sources)
        });
        let mut watcher = Self {
            path,
            sources,
            stamps: Vec::new(),
        };
        watcher.stamps = watcher.snapshot();
        watcher
    }
    fn snapshot(&self) -> Vec<Option<(SystemTime, u64)>> {
        self.sources
            .iter()
            .map(|path| {
                std::fs::metadata(path)
                    .ok()
                    .and_then(|m| m.modified().ok().map(|time| (time, m.len())))
            })
            .collect()
    }
    pub fn changed(&mut self) -> bool {
        let next = self.snapshot();
        let changed = next != self.stamps;
        self.stamps = next;
        changed
    }
    pub fn loaded(&mut self, sources: Vec<PathBuf>) {
        self.sources = sources;
        self.stamps = self.snapshot();
    }
}
