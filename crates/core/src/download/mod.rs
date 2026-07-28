pub mod cache;
pub mod cleanroom;
pub mod fabric;
pub mod fetch;
pub mod forge;
pub mod forge_old;
pub mod legacyfabric;
pub mod liteloader;
pub mod modrinth;
pub mod mojang_java;
pub mod neoforge;
pub mod optifine;
mod provider;
pub mod quilt;

pub use cache::CacheRepository;
pub use fetch::{
    fetch_all_cached, fetch_all_cached_with_progress, fetch_to_file, fetch_to_file_cached,
    fetch_to_file_with_progress, Expected, FetchError, FetchJob, InstallStage, ProgressEvent,
    ProgressSink,
};
pub use provider::DownloadProvider;

pub const DEFAULT_BMCLAPI_API_ROOT: &str = "https://bmclapi2.bangbang93.com";
