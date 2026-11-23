//! models.dev data, kept in memory and as one file on disk.
//!
//! A preview downloads fresh data when there is no copy or the copy is older
//! than a day, or when the user asks for it. If the download fails, the saved
//! copy is used and marked as old. Everything runs inside the caller's request,
//! with no threads or timers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::catalog::Catalog;
use crate::error::PluginError;
use crate::host;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const CACHE_FOLDER_NAME: &str = "cpa-plugin-fill-model-details";
const CACHE_FILE_NAME: &str = "models-dev-api.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// Download when there is no copy or the copy is older than a day.
    WhenOld,
    /// Always download; keep the old copy if the download fails.
    Always,
    /// Never download; use only the copy already in memory or on disk.
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CatalogStatus {
    pub downloaded_at_unix_seconds: u64,
    pub model_count: usize,
    pub is_old: bool,
    /// Why the download failed, when one was tried and failed.
    pub download_error: Option<String>,
    /// Why the copy could not be saved on disk, when it could not.
    pub save_error: Option<String>,
}

pub struct LoadedCatalog {
    pub catalog: Arc<Catalog>,
    pub status: CatalogStatus,
}

struct CachedCatalog {
    catalog: Arc<Catalog>,
    downloaded_at: SystemTime,
}

/// One lock for memory and disk, so two previews never download at once.
static CACHE: Mutex<Option<CachedCatalog>> = Mutex::new(None);

pub fn load(
    refresh: Refresh,
    host_callback_id: Option<&str>,
) -> Result<LoadedCatalog, PluginError> {
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let cache_path = cache_file_path();
    if cache.is_none()
        && let Some(path) = &cache_path
    {
        *cache = read_copy(path);
    }

    let should_download = match refresh {
        Refresh::Always => true,
        Refresh::Never => false,
        Refresh::WhenOld => cache
            .as_ref()
            .is_none_or(|cached| is_old(cached.downloaded_at)),
    };
    let mut download_error = None;
    let mut save_error = None;
    if should_download {
        match download(host_callback_id) {
            Ok((catalog, bytes)) => {
                save_error = match &cache_path {
                    Some(path) => write_copy(path, &bytes)
                        .err()
                        .map(|error| error.to_string()),
                    None => Some("no folder for the saved copy was found".to_string()),
                };
                *cache = Some(CachedCatalog {
                    catalog: Arc::new(catalog),
                    downloaded_at: SystemTime::now(),
                });
            }
            Err(error) => download_error = Some(error.message),
        }
    }

    let Some(cached) = cache.as_ref() else {
        let message = download_error
            .unwrap_or_else(|| "models.dev data has not been downloaded yet".to_string());
        return Err(PluginError::new("catalog_unavailable", message));
    };
    Ok(LoadedCatalog {
        catalog: Arc::clone(&cached.catalog),
        status: CatalogStatus {
            downloaded_at_unix_seconds: unix_seconds(cached.downloaded_at),
            model_count: cached.catalog.model_count(),
            is_old: is_old(cached.downloaded_at),
            download_error,
            save_error,
        },
    })
}

/// Downloads through CPA's HTTP client, so CPA's proxy settings apply.
fn download(host_callback_id: Option<&str>) -> Result<(Catalog, Vec<u8>), PluginError> {
    let response = host::http_get(MODELS_DEV_URL, host_callback_id)?;
    if response.status_code != 200 {
        return Err(PluginError::new(
            "download_failed",
            format!("models.dev answered HTTP {}", response.status_code),
        ));
    }
    let catalog = Catalog::parse(&response.body)?;
    Ok((catalog, response.body))
}

fn cache_file_path() -> Option<PathBuf> {
    let base_folder = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base_folder.join(CACHE_FOLDER_NAME).join(CACHE_FILE_NAME))
}

/// A broken or missing copy is ignored, so the next preview downloads again.
fn read_copy(path: &Path) -> Option<CachedCatalog> {
    let bytes = fs::read(path).ok()?;
    let catalog = Catalog::parse(&bytes).ok()?;
    let downloaded_at = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(UNIX_EPOCH);
    Some(CachedCatalog {
        catalog: Arc::new(catalog),
        downloaded_at,
    })
}

/// Writes a temporary file first, so a failed write never leaves half a copy.
fn write_copy(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    let temporary_path = path.with_extension("json.tmp");
    fs::write(&temporary_path, bytes)?;
    fs::rename(&temporary_path, path)
}

fn is_old(downloaded_at: SystemTime) -> bool {
    SystemTime::now()
        .duration_since(downloaded_at)
        .is_ok_and(|age| age > MAX_AGE)
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_copy_round_trip() {
        let folder =
            std::env::temp_dir().join(format!("fill-model-details-test-{}", std::process::id()));
        let path = folder.join(CACHE_FILE_NAME);
        let bytes = include_bytes!("../tests/fixtures/models-dev-sample.json");

        write_copy(&path, bytes).expect("the copy should be written");
        let cached = read_copy(&path).expect("the copy should be read back");
        assert_eq!(cached.catalog.model_count(), 14);
        assert!(!is_old(cached.downloaded_at));
        assert!(!path.with_extension("json.tmp").exists());

        fs::write(&path, b"not json").expect("overwrite with broken data");
        assert!(read_copy(&path).is_none());
        fs::remove_dir_all(&folder).expect("clean up the test folder");
    }

    #[test]
    fn age_check() {
        let two_days_ago = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        assert!(is_old(two_days_ago));
        assert!(!is_old(SystemTime::now()));
        assert_eq!(unix_seconds(UNIX_EPOCH + Duration::from_secs(42)), 42);
    }
}
