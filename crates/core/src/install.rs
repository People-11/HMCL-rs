use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::Deserialize;

use crate::download::fetch::fetch_to_file;
use crate::download::{
    fetch_to_file_cached, CacheRepository, DownloadProvider, Expected, FetchError,
};
use crate::version::{Artifact, Env, Library, Version};

pub struct GameRepository {
    pub root: PathBuf,
}

impl GameRepository {
    /// `root` 会被转成绝对路径（不要求目录已存在, 所以不能用 `canonicalize()`——
    /// 装一个全新的 `.minecraft` 时目录本来就还不存在）。这不是可有可无的规范化:
    /// 启动进程时 `cmd.current_dir()` 会被设成跟这里同一个 `root`, 如果 `root` 是
    /// 相对路径（比如用户直接敲 `--dir .minecraft`）, 子进程的 cwd 变成
    /// `父进程cwd/.minecraft` 之后, 再拿这里生成的、同样带着 "`.minecraft/`" 前缀的
    /// classpath/natives 相对路径去给子进程用, 就会被再解析一层, 变成
    /// `父进程cwd/.minecraft/.minecraft/...`——一个真实存在过的 bug: Forge 新版的
    /// `-p` 模块路径找不到文件时只是静默把模块从解析结果里剔除（不像 `-cp` 主类
    /// 找不到那样好歹报一个 `ClassNotFoundException`），现象一度看起来像 Forge
    /// processor 本身有问题，实际上普通 `-cp`（vanilla/Fabric）一样会中招，只是
    /// 之前测试一直凑巧传的是绝对路径的 `--dir` 才没暴露出来。
    pub fn new(root: impl Into<PathBuf>) -> GameRepository {
        let root = root.into();
        let root = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(&root))
                .unwrap_or(root)
        };
        GameRepository { root }
    }

    pub fn version_root(&self, id: &str) -> PathBuf {
        self.root.join("versions").join(id)
    }

    pub fn version_jar(&self, id: &str) -> PathBuf {
        self.version_root(id).join(format!("{id}.jar"))
    }

    pub fn version_json_path(&self, id: &str) -> PathBuf {
        self.version_root(id).join(format!("{id}.json"))
    }

    pub fn libraries_dir(&self) -> PathBuf {
        self.root.join("libraries")
    }

    pub fn library_file(&self, lib: &Library, env: Env) -> PathBuf {
        self.libraries_dir().join(lib.path(env))
    }

    pub fn artifact_file(&self, artifact: &Artifact) -> PathBuf {
        self.libraries_dir().join(artifact.path())
    }

    pub fn assets_dir(&self) -> PathBuf {
        self.root.join("assets")
    }

    pub fn clear_shared_assets(&self, id: &str) -> std::io::Result<()> {
        remove_directory_if_present(&self.assets_dir())?;
        remove_directory_if_present(&self.run_directory(id).join("resources"))
    }

    pub fn clear_shared_libraries(&self) -> std::io::Result<()> {
        remove_directory_if_present(&self.libraries_dir())
    }

    pub fn clean_instance_logs(&self, id: &str) -> std::io::Result<()> {
        let run_dir = self.run_directory(id);
        for root in [&self.root, &run_dir] {
            remove_directory_if_present(&root.join("logs"))?;
            remove_directory_if_present(&root.join("crash-reports"))?;
        }
        Ok(())
    }

    pub fn asset_indexes_dir(&self) -> PathBuf {
        self.assets_dir().join("indexes")
    }

    pub fn asset_index_file(&self, index_id: &str) -> PathBuf {
        self.asset_indexes_dir().join(format!("{index_id}.json"))
    }

    pub fn asset_objects_dir(&self) -> PathBuf {
        self.assets_dir().join("objects")
    }

    pub fn asset_object_file(&self, hash: &str) -> PathBuf {
        self.asset_objects_dir()
            .join(&hash[..hash.len().min(2)])
            .join(hash)
    }

    /// 对应 Java `DefaultGameRepository.getActualAssetDirectory`：老版本（`is_virtual`）
    /// 用的是 `assets/virtual/{indexId}/`，现代版本直接就是 `assets/`。
    pub fn actual_asset_directory(&self, index_id: &str, is_virtual: bool) -> PathBuf {
        if is_virtual {
            self.assets_dir().join("virtual").join(index_id)
        } else {
            self.assets_dir()
        }
    }

    pub fn run_directory(&self, id: &str) -> PathBuf {
        let settings = crate::settings::instance_game_settings::load(self, id);
        settings.run_directory(self, id, self.is_modpack(id))
    }

    pub fn is_modpack(&self, id: &str) -> bool {
        self.version_root(id).join("modpack.cfg").is_file()
    }

    pub fn native_directory(&self, id: &str, platform: crate::platform::Platform) -> PathBuf {
        self.version_root(id).join(format!("natives-{platform}"))
    }

    pub fn classpath(&self, version: &Version, env: Env) -> Vec<String> {
        let mut seen = HashSet::new();
        version
            .libraries
            .iter()
            .filter(|lib| lib.applies_to(env) && !lib.is_native(env))
            .map(|lib| self.library_file(lib, env))
            .filter(|p| p.is_file())
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|s| seen.insert(s.clone()))
            .collect()
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    pub fn save_version_json(&self, version: &Version) -> std::io::Result<()> {
        let path = self.version_json_path(&version.id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(version).expect("Version must always serialize");
        std::fs::write(path, text)
    }

    /// 扫描 `versions/*/` 找出所有"实例"的 id——对应 Java
    /// `DefaultGameRepository.refreshVersionsImpl()` 里"列出 versions 目录下的
    /// 子目录"那一步。
    ///
    /// ponytail: 没有照抄 Java 版那几条自愈逻辑（子目录里恰好只有一个 `.json`
    /// 但文件名跟目录名对不上时自动改名；解析出来的 `Version.id` 跟目录名不一致
    /// 时把目录也重命名成一致）——这些是"用户手动改过文件名"的兜底容错，不影响
    /// 正常场景下"发现我们自己刚创建的实例"这个核心需求。这里只认
    /// `{目录名}/{目录名}.json` 这一种规范形式，找不到就跳过，不做自动修复。
    pub fn list_instance_ids(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.versions_dir()) else {
            return Vec::new();
        };
        entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|id| self.version_json_path(id).is_file())
            .collect()
    }

    pub fn load_all_versions(&self) -> HashMap<String, Version> {
        let mut versions = HashMap::new();
        for id in self.list_instance_ids() {
            let path = self.version_json_path(&id);
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<Version>(&text).ok())
            {
                Some(version) => {
                    versions.insert(id, version);
                }
                None => {
                    tracing::warn!(path = %path.display(), "failed to parse version.json, skipping this instance")
                }
            }
        }
        versions
    }

    /// 对应 Java `HMCLGameRepository.getRunDirectory`（不是 `DefaultGameRepository`
    /// 那个恒等于游戏根目录的默认实现）：真正实现"实例隔离"的版本。
    ///
    /// - `running_directory_override` 是实例设置里 `runningDirectory` 字段的值,
    ///   `overridden` 对应 `overrideProperties` 里有没有列这个属性名——两者要
    ///   分开传是因为 Java 原版这里有三种状态而不是两种：`Some("")` (显式覆盖成
    ///   空字符串) 的含义是"隔离到这个实例自己的目录", 跟"完全没有覆盖这个属性"
    ///   （用共享根目录）是两回事,不能用 `Option<&str>` 一个参数就地表达。
    /// - `is_modpack` 的实例（`versions/{id}/modpack.cfg` 存在）无条件用自己的
    ///   目录，不管 `runningDirectory` 设了什么。
    pub fn run_directory_isolated(
        &self,
        id: &str,
        overridden: bool,
        running_directory_override: &str,
        is_modpack: bool,
    ) -> PathBuf {
        if is_modpack {
            return self.version_root(id);
        }
        if !overridden {
            return self.root.clone();
        }
        if running_directory_override.is_empty() {
            self.version_root(id)
        } else {
            PathBuf::from(running_directory_override)
        }
    }
}

fn remove_directory_if_present(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionManifestEntry {
    pub id: String,
    pub url: String,
    #[serde(default, rename = "type")]
    pub release_type: Option<crate::version::ReleaseType>,
    #[serde(default, rename = "releaseTime")]
    pub release_time: Option<String>,
    /// 这个版本 `version.json` 的 sha1。只有 v2 版 manifest 有这个字段
    /// （我们默认就请求 v2，见 `DownloadProvider::mojang`），所以类型是
    /// `Option`——万一哪天回落到 v1 或者镜像少给了这一项，就退化成不校验，
    /// 而不是直接下不动。
    #[serde(default)]
    pub sha1: Option<String>,
}

impl VersionManifestEntry {
    /// 把 `releaseTime` 拆成 `(年, 月, 日, 时, 分, 秒)` 六段字符串，给 UI 拼显示
    /// 文本用。
    ///
    /// ponytail: 直接按固定偏移切字符串，没有引入 `chrono`/`time` 这种日期库，
    /// 也**没有做时区换算**——Mojang 给的是 UTC（`+00:00`），这里就原样显示 UTC，
    /// 而真实 HMCL 显示的是本机时区，所以东八区看到的时刻会比 HMCL 早 8 小时。
    /// 只为了版本列表里一行只读的时间戳就上一个日期库 + 本地时区探测不划算；
    /// 真要对齐再换成 `time` crate 的 `OffsetDateTime` + `UtcOffset::current_local_offset`。
    pub fn release_date_parts(&self) -> Option<(&str, &str, &str, &str, &str, &str)> {
        let s = self.release_time.as_deref()?;
        if s.len() < 19 || !s.is_char_boundary(19) {
            return None;
        }
        Some((
            &s[0..4],
            &s[5..7],
            &s[8..10],
            &s[11..13],
            &s[14..16],
            &s[17..19],
        ))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionManifest {
    pub versions: Vec<VersionManifestEntry>,
}

impl VersionManifest {
    pub fn find(&self, id: &str) -> Option<&VersionManifestEntry> {
        self.versions.iter().find(|v| v.id == id)
    }
}

pub async fn fetch_version_manifest(
    client: &reqwest::Client,
    provider: &DownloadProvider,
) -> Result<VersionManifest, InstallError> {
    let mut last_error = None;
    for url in provider.version_manifest_candidates() {
        match client
            .get(url)
            .send()
            .await
            .and_then(|response| response.error_for_status())
        {
            Ok(response) => match response.text().await {
                Ok(text) => return Ok(serde_json::from_str(&text)?),
                Err(error) => last_error = Some(error),
            },
            Err(error) => last_error = Some(error),
        }
    }
    Err(FetchError::Http(last_error.expect("download provider always has a manifest URL")).into())
}

/// 下载某个具体版本的 version.json 并解析。`url` 通常来自
/// [`VersionManifest::find`] 找到的那一条。
/// 下载并解析一个版本的 `version.json`。
///
/// 收整个 manifest 条目而不是光一个 URL，是为了拿到条目里的 `sha1` 做校验——
/// 那是官方唯一一处能验 `version.json` 的哈希。条目没带 sha1（老的 v1 manifest）
/// 时退化成不校验，行为跟以前一样。
pub async fn download_version_json(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    repo: &GameRepository,
    id: &str,
    entry: &VersionManifestEntry,
) -> Result<Version, InstallError> {
    let candidates = provider.inject_url_candidates(&entry.url);
    let dest = repo.version_json_path(id);
    let expected = match &entry.sha1 {
        Some(sha1) => Expected::sha1(sha1.clone()),
        None => Expected::default(),
    };
    fetch_to_file(client, &candidates, &dest, &expected).await?;
    let text = tokio::fs::read_to_string(&dest).await?;
    Ok(serde_json::from_str(&text)?)
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AssetIndexFile {
    #[serde(default)]
    pub objects: HashMap<String, AssetObject>,
    #[serde(default)]
    pub r#virtual: bool,
    #[serde(default, rename = "map_to_resources")]
    pub map_to_resources: bool,
}

impl AssetIndexFile {
    pub fn is_virtual(&self) -> bool {
        self.r#virtual || self.map_to_resources
    }
}

fn expected_from(url: Option<&str>, sha1: Option<&str>, size: u64) -> Option<(String, Expected)> {
    let url = url?;
    Some((
        url.to_string(),
        Expected {
            sha1: sha1.map(|s| s.to_string()),
            size: if size > 0 { Some(size) } else { None },
        },
    ))
}

pub async fn install_client_jar(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    version: &Version,
) -> Result<(), FetchError> {
    let info = version.client_download_info();
    let (url, expected) = expected_from(info.url.as_deref(), info.checksum(), info.size)
        .ok_or(FetchError::NoCandidates)?;
    let candidates = provider.inject_url_candidates(&url);
    let dest = repo.version_jar(&version.id);
    fetch_to_file_cached(client, cache, &candidates, &dest, &expected).await
}

/// 下载所有适用于当前平台的 library（natives 也算在内）到 `libraries/`。
/// 返回每个任务的结果——单个库下载失败不会让其它库的下载停下来，调用方自己
/// 决定"有失败就整体判失败"还是"允许部分失败重试"。
///
/// 对应 Java `GameLibrariesTask` 里那行容易看漏的条件：
/// `shouldDownloadLibrary(...) && (library.hasDownloadURL() || !"optifine".equals(library.getGroupId()))`——
/// 也就是说"没有显式下载 URL 就不下载"这条规则**只对 OptiFine 的库生效**
/// （OptiFine 的库是从它自己的安装器里解出来的，从来没有真正可下载的 URL）。
/// 对所有其它库（包括老版本 Forge 那些只写了坐标、没写 `url`/`downloads` 的
/// 库，比如 `net.minecraft:launchwrapper:1.12`），即使没有显式 URL 也必须尝试
/// 下载——`Library::download()` 会自动落回 `DEFAULT_LIBRARY_URL`。这是用真实
/// Forge 1.7.10 安装验证时才炸出来的真 bug：之前把"没有显式 URL 就跳过"当成
/// 对所有库都成立的规则，导致这类库直接被静默漏下载，classpath 里缺了
/// `launchwrapper`，`ClassNotFoundException` 报的还正好是主类本身，比较唬人。
fn allowed_without_explicit_url(lib: &Library) -> bool {
    lib.artifact.group != "optifine"
}

pub async fn install_libraries(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    version: &Version,
    env: Env<'_>,
) -> Vec<(PathBuf, Result<(), FetchError>)> {
    install_libraries_with_progress(client, provider, cache, repo, version, env, None).await
}

pub async fn install_libraries_with_progress(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    version: &Version,
    env: Env<'_>,
    progress: Option<&crate::download::ProgressSink>,
) -> Vec<(PathBuf, Result<(), FetchError>)> {
    let jobs = version
        .libraries
        .iter()
        .filter(|lib| {
            lib.applies_to(env) && (lib.has_download_url(env) || allowed_without_explicit_url(lib))
        })
        .filter_map(|lib| {
            let download = lib.download(env);
            let (url, expected) = expected_from(
                download.download.url.as_deref(),
                download.download.checksum(),
                download.download.size,
            )?;
            Some(crate::download::FetchJob {
                candidates: provider.inject_url_candidates(&url),
                dest: repo.library_file(lib, env),
                expected,
            })
        })
        .collect();

    let progress = progress.map(|tx| (crate::download::InstallStage::Libraries, tx));
    crate::download::fetch_all_cached_with_progress(
        client,
        cache,
        jobs,
        provider.concurrency(),
        progress,
    )
    .await
}

/// 下载 assets index json 并解析出 objects 表。用不缓存的 `fetch_to_file` 而不是
/// `fetch_to_file_cached`：老版本的 legacy assetIndex 回落项没有 sha1（见
/// `Version::asset_index` 的说明），缓存是按 sha1 建索引的，没有 key 就没法查缓存。
pub async fn install_asset_index(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    repo: &GameRepository,
    version: &Version,
) -> Result<AssetIndexFile, InstallError> {
    let info = version.asset_index();
    let (url, expected) = expected_from(
        info.base.download.url.as_deref(),
        info.base.download.checksum(),
        info.base.download.size,
    )
    .ok_or(FetchError::NoCandidates)?;
    let dest = repo.asset_index_file(&info.base.id);
    let candidates = provider.inject_url_candidates(&url);

    fetch_to_file(client, &candidates, &dest, &expected).await?;

    let text = tokio::fs::read_to_string(&dest).await?;
    let parsed: AssetIndexFile = serde_json::from_str(&text)?;
    Ok(parsed)
}

#[allow(clippy::too_many_arguments)]
pub async fn install_asset_objects_with_progress(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    index: &AssetIndexFile,
    concurrency: usize,
    progress: Option<&crate::download::ProgressSink>,
) -> Vec<(PathBuf, Result<(), FetchError>)> {
    let jobs = index
        .objects
        .values()
        .map(|obj| {
            let location = format!("{}/{}", &obj.hash[..obj.hash.len().min(2)], obj.hash);
            crate::download::FetchJob {
                candidates: provider.asset_object_candidates(&location),
                dest: repo.asset_object_file(&obj.hash),
                expected: Expected::sha1_and_size(obj.hash.clone(), obj.size),
            }
        })
        .collect();

    let progress = progress.map(|tx| (crate::download::InstallStage::AssetObjects, tx));
    crate::download::fetch_all_cached_with_progress(client, cache, jobs, concurrency, progress)
        .await
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse asset index: {0}")]
    Json(#[from] serde_json::Error),
}

/// 端到端安装一个已经 resolve() 过的版本：jar + libraries + assets index + assets
/// objects。任何一步的"整体性"失败（网络完全不可用、index 解析失败）会短路返回错误；
/// 单个库/单个 asset 文件下载失败不会——那些失败项会汇总在返回值里，由调用方决定
/// 要不要重试或者提示用户。
pub async fn install_version(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    version: &Version,
    env: Env<'_>,
) -> Result<InstallReport, InstallError> {
    install_version_with_progress(client, provider, cache, repo, version, env, None).await
}

#[allow(clippy::too_many_arguments)]
pub async fn install_version_with_progress(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    version: &Version,
    env: Env<'_>,
    progress: Option<&crate::download::ProgressSink>,
) -> Result<InstallReport, InstallError> {
    install_client_jar(client, provider, cache, repo, version).await?;
    let library_results =
        install_libraries_with_progress(client, provider, cache, repo, version, env, progress)
            .await;
    let asset_index = install_asset_index(client, provider, repo, version).await?;
    let object_results = install_asset_objects_with_progress(
        client,
        provider,
        cache,
        repo,
        &asset_index,
        provider.concurrency(),
        progress,
    )
    .await;

    Ok(InstallReport {
        library_results,
        object_results,
    })
}

#[derive(Debug)]
pub struct InstallReport {
    pub library_results: Vec<(PathBuf, Result<(), FetchError>)>,
    pub object_results: Vec<(PathBuf, Result<(), FetchError>)>,
}

impl InstallReport {
    pub fn is_complete_success(&self) -> bool {
        self.library_results.iter().all(|(_, r)| r.is_ok())
            && self.object_results.iter().all(|(_, r)| r.is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::fetch::sha1_hex;
    use crate::platform::Platform;
    use crate::version::{Artifact, Library};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test-install")
            .join(name)
            .join(format!("{:x}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn library_with_url(server_uri: &str, coord: &str, rel_path: &str, body: &[u8]) -> Library {
        Library {
            artifact: Artifact::from_descriptor(coord).unwrap(),
            url: Some(format!("{server_uri}/maven/")),
            downloads: Some(crate::version::LibrariesDownloadInfo {
                artifact: Some(crate::version::LibraryDownloadInfo {
                    path: Some(rel_path.to_string()),
                    download: crate::version::DownloadInfo {
                        url: Some(format!("{server_uri}/maven/{rel_path}")),
                        sha1: Some(sha1_hex(body)),
                        size: body.len() as u64,
                    },
                }),
                classifiers: HashMap::new(),
            }),
            extract: None,
            natives: None,
            rules: Vec::new(),
            checksums: None,
            hint: None,
            file_name: None,
        }
    }

    #[tokio::test]
    async fn installs_client_jar_libraries_and_assets_end_to_end() {
        let server = MockServer::start().await;
        let jar_body = b"fake client jar bytes".to_vec();
        let lib_body = b"fake library bytes".to_vec();
        let asset_body = b"fake asset bytes".to_vec();
        let asset_hash = sha1_hex(&asset_body);

        Mock::given(method("GET"))
            .and(path("/client.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(jar_body.clone()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/maven/org/example/thing/1.0/thing-1.0.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(lib_body.clone()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/assets/index.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "objects": {
                    "icons/icon.png": { "hash": asset_hash, "size": asset_body.len() }
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/assets/{}/{}", &asset_hash[..2], asset_hash)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(asset_body.clone()))
            .mount(&server)
            .await;

        let mut version = Version::new("test-version");
        version.jar = Some("test-version".to_string());
        version.downloads.get_or_insert_with(HashMap::new).insert(
            crate::version::DownloadType::Client,
            crate::version::DownloadInfo {
                url: Some(format!("{}/client.jar", server.uri())),
                sha1: Some(sha1_hex(&jar_body)),
                size: jar_body.len() as u64,
            },
        );
        version.libraries = vec![library_with_url(
            &server.uri(),
            "org.example:thing:1.0",
            "org/example/thing/1.0/thing-1.0.jar",
            &lib_body,
        )];
        version.asset_index = Some(crate::version::AssetIndexInfo {
            base: crate::version::IdDownloadInfo {
                id: "index".to_string(),
                download: crate::version::DownloadInfo {
                    url: Some(format!("{}/assets/index.json", server.uri())),
                    sha1: None,
                    size: 0,
                },
            },
            total_size: 0,
        });

        let root = tmp_dir("end_to_end");
        let repo = GameRepository::new(&root);
        let provider = DownloadProvider::bmclapi(server.uri());
        let cache = CacheRepository::new(root.join("cache"));
        let client = reqwest::Client::new();
        let env = Env {
            platform: Platform::WINDOWS_X64,
            os_version: "",
        };

        let report = install_version(&client, &provider, &cache, &repo, &version, env)
            .await
            .expect("install pipeline should succeed end to end");

        assert!(
            report.is_complete_success(),
            "no per-file failures expected"
        );
        assert_eq!(
            tokio::fs::read(repo.version_jar("test-version"))
                .await
                .unwrap(),
            jar_body
        );
        assert_eq!(
            tokio::fs::read(
                repo.libraries_dir()
                    .join("org/example/thing/1.0/thing-1.0.jar")
            )
            .await
            .unwrap(),
            lib_body
        );
        assert_eq!(
            tokio::fs::read(repo.asset_object_file(&asset_hash))
                .await
                .unwrap(),
            asset_body
        );

        let dest2 = root.join("another-instance-libs").join("thing-1.0.jar");
        let hit = cache
            .link_from_cache(&sha1_hex(&lib_body), &dest2)
            .await
            .unwrap();
        assert!(
            hit,
            "second instance should reuse the cached library, not re-download"
        );
        assert_eq!(tokio::fs::read(&dest2).await.unwrap(), lib_body);
    }

    #[tokio::test]
    async fn library_excluded_by_os_rule_is_not_downloaded() {
        let server = MockServer::start().await;
        let mut lib = library_with_url(
            &server.uri(),
            "org.example:mac-only:1.0",
            "org/example/mac-only/1.0/x.jar",
            b"x",
        );
        lib.rules = vec![crate::version::CompatibilityRule {
            action: crate::version::RuleAction::Allow,
            os: Some(crate::version::OsRestriction {
                name: Some("osx".to_string()),
                version: None,
                arch: None,
            }),
            features: None,
        }];

        let mut version = Version::new("test-version");
        version.libraries = vec![lib];

        let root = tmp_dir("os_excluded");
        let repo = GameRepository::new(&root);
        let provider = DownloadProvider::mojang();
        let cache = CacheRepository::new(root.join("cache"));
        let client = reqwest::Client::new();
        let env = Env {
            platform: Platform::WINDOWS_X64,
            os_version: "",
        };

        let results = install_libraries(&client, &provider, &cache, &repo, &version, env).await;
        assert!(
            results.is_empty(),
            "a macOS-only library must be skipped entirely on Windows, not attempted and failed"
        );
    }

    #[tokio::test]
    async fn library_with_no_explicit_url_still_falls_back_to_the_default_repo() {
        let server = MockServer::start().await;
        let body = b"fake launchwrapper bytes".to_vec();
        Mock::given(method("GET"))
            .and(path(
                "/libraries/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let bare_lib = Library {
            artifact: Artifact::from_descriptor("net.minecraft:launchwrapper:1.12").unwrap(),
            url: None,
            downloads: None,
            extract: None,
            natives: None,
            rules: Vec::new(),
            checksums: None,
            hint: None,
            file_name: None,
        };

        let mut version = Version::new("test-version");
        version.libraries = vec![bare_lib];

        let root = tmp_dir("no_explicit_url_fallback");
        let repo = GameRepository::new(&root);
        let provider = DownloadProvider::bmclapi(server.uri());
        let cache = CacheRepository::new(root.join("cache"));
        let client = reqwest::Client::new();
        let env = Env {
            platform: Platform::WINDOWS_X64,
            os_version: "",
        };

        let results = install_libraries(&client, &provider, &cache, &repo, &version, env).await;
        assert_eq!(results.len(), 1, "a library with no explicit url/downloads must still be attempted, not silently skipped");
        assert!(
            results[0].1.is_ok(),
            "download must succeed via the DEFAULT_LIBRARY_URL fallback: {:?}",
            results[0].1
        );
    }

    #[tokio::test]
    async fn optifine_library_with_no_explicit_url_is_skipped() {
        let server = MockServer::start().await;

        let optifine_lib = Library {
            artifact: Artifact::from_descriptor("optifine:OptiFine:1.20.1_HD_U_I6").unwrap(),
            url: None,
            downloads: None,
            extract: None,
            natives: None,
            rules: Vec::new(),
            checksums: None,
            hint: None,
            file_name: None,
        };

        let mut version = Version::new("test-version");
        version.libraries = vec![optifine_lib];

        let root = tmp_dir("optifine_no_url_skipped");
        let repo = GameRepository::new(&root);
        let provider = DownloadProvider::bmclapi(server.uri());
        let cache = CacheRepository::new(root.join("cache"));
        let client = reqwest::Client::new();
        let env = Env {
            platform: Platform::WINDOWS_X64,
            os_version: "",
        };

        let results = install_libraries(&client, &provider, &cache, &repo, &version, env).await;
        assert!(
            results.is_empty(),
            "an OptiFine library with no explicit url must be skipped, not attempted and failed"
        );
    }

    #[test]
    fn classpath_deduplicates_by_resolved_path_like_javas_linkedhashset() {
        let root = tmp_dir("classpath_dedup");
        std::fs::create_dir_all(root.join("libraries/net/sf/jopt-simple/jopt-simple/5.0.4"))
            .unwrap();
        std::fs::write(
            root.join("libraries/net/sf/jopt-simple/jopt-simple/5.0.4/jopt-simple-5.0.4.jar"),
            b"fake",
        )
        .unwrap();

        let repo = GameRepository::new(&root);
        let dup_lib = |version: &str| Library {
            artifact: Artifact::from_descriptor(&format!(
                "net.sf.jopt-simple:jopt-simple:{version}"
            ))
            .unwrap(),
            url: None,
            downloads: None,
            extract: None,
            natives: None,
            rules: Vec::new(),
            checksums: None,
            hint: None,
            file_name: None,
        };

        let mut version = Version::new("test-version");
        version.libraries = vec![dup_lib("5.0.4"), dup_lib("5.0.4")];

        let env = Env {
            platform: Platform::WINDOWS_X64,
            os_version: "",
        };
        let cp = repo.classpath(&version, env);
        assert_eq!(
            cp.len(),
            1,
            "duplicate resolved paths must collapse to a single classpath entry, got {cp:?}"
        );
    }

    #[test]
    fn save_version_json_writes_the_unresolved_recipe_not_a_merged_result() {
        let root = tmp_dir("save_version_json");
        let repo = GameRepository::new(&root);

        let mut instance = Version::new("1.20.1-forge");
        instance.inherits_from = Some("1.20.1".to_string());
        let mut patch = Version::new("forge");
        patch.priority = Some(Version::PRIORITY_LOADER);
        instance.patches = Some(vec![patch]);

        repo.save_version_json(&instance).unwrap();

        let text = std::fs::read_to_string(repo.version_json_path("1.20.1-forge")).unwrap();
        let read_back: Version = serde_json::from_str(&text).unwrap();
        assert_eq!(
            read_back.inherits_from.as_deref(),
            Some("1.20.1"),
            "must persist the recipe (inheritsFrom), not a resolved/flattened version"
        );
        assert_eq!(read_back.patches.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn list_instance_ids_finds_only_directories_with_a_matching_json() {
        let root = tmp_dir("list_instance_ids");
        let repo = GameRepository::new(&root);

        repo.save_version_json(&Version::new("1.20.1")).unwrap();
        repo.save_version_json(&Version::new("1.20.1-forge"))
            .unwrap();
        std::fs::create_dir_all(repo.version_root("half-installed")).unwrap();

        let mut ids = repo.list_instance_ids();
        ids.sort();
        assert_eq!(ids, vec!["1.20.1".to_string(), "1.20.1-forge".to_string()]);
    }

    #[test]
    fn load_all_versions_resolves_a_persisted_instance_against_its_persisted_parent() {
        let root = tmp_dir("load_all_versions");
        let repo = GameRepository::new(&root);

        let mut vanilla = Version::new("1.20.1");
        vanilla.main_class = Some("net.minecraft.client.main.Main".to_string());
        repo.save_version_json(&vanilla).unwrap();

        let mut instance = Version::new("1.20.1-forge");
        instance.inherits_from = Some("1.20.1".to_string());
        let mut patch = Version::new("forge");
        patch.priority = Some(Version::PRIORITY_LOADER);
        patch.main_class = Some("cpw.mods.bootstraplauncher.BootstrapLauncher".to_string());
        instance.patches = Some(vec![patch]);
        repo.save_version_json(&instance).unwrap();

        let all = repo.load_all_versions();
        assert_eq!(all.len(), 2);

        let resolved = all
            .get("1.20.1-forge")
            .unwrap()
            .resolve(&all)
            .expect("must resolve against the sibling versions loaded from disk");
        assert_eq!(
            resolved.main_class.as_deref(),
            Some("cpw.mods.bootstraplauncher.BootstrapLauncher")
        );
    }

    #[test]
    fn load_all_versions_skips_unparseable_files_instead_of_failing_everything() {
        let root = tmp_dir("load_all_versions_skips_bad");
        let repo = GameRepository::new(&root);

        repo.save_version_json(&Version::new("good")).unwrap();
        std::fs::create_dir_all(repo.version_root("bad")).unwrap();
        std::fs::write(repo.version_json_path("bad"), "{ not valid json").unwrap();

        let all = repo.load_all_versions();
        assert_eq!(all.len(), 1);
        assert!(all.contains_key("good"));
    }

    #[test]
    fn run_directory_reads_the_persisted_instance_settings_from_disk() {
        let root = tmp_dir("run_directory_reads_settings");
        let repo = GameRepository::new(&root);

        assert_eq!(repo.run_directory("no-settings-file"), repo.root);

        use crate::settings::instance_game_settings::{
            InstanceGameSettings, PROPERTY_RUNNING_DIRECTORY,
        };
        let mut settings = InstanceGameSettings {
            running_directory: Some(String::new()),
            ..Default::default()
        };
        settings.set_overridden(PROPERTY_RUNNING_DIRECTORY);
        let path =
            crate::settings::instance_game_settings::instance_settings_path(&repo, "isolated");
        crate::settings::save(
            &path,
            crate::settings::instance_game_settings::SCHEMA_ID,
            &settings,
        )
        .unwrap();

        assert_eq!(
            repo.run_directory("isolated"),
            repo.version_root("isolated")
        );
    }

    #[test]
    fn run_directory_ignores_settings_override_for_a_modpack_instance() {
        let root = tmp_dir("run_directory_modpack");
        let repo = GameRepository::new(&root);

        std::fs::create_dir_all(repo.version_root("pack")).unwrap();
        std::fs::write(repo.version_root("pack").join("modpack.cfg"), "").unwrap();

        assert!(repo.is_modpack("pack"));
        assert!(!repo.is_modpack("no-modpack-cfg-here"));
        assert_eq!(repo.run_directory("pack"), repo.version_root("pack"));
    }

    #[test]
    fn shared_cleanup_only_removes_the_requested_runtime_data() {
        let root = tmp_dir("shared_cleanup");
        let repo = GameRepository::new(&root);
        repo.save_version_json(&Version::new("test")).unwrap();
        std::fs::create_dir_all(repo.assets_dir()).unwrap();
        std::fs::create_dir_all(repo.libraries_dir()).unwrap();
        std::fs::create_dir_all(root.join("logs")).unwrap();
        std::fs::create_dir_all(root.join("crash-reports")).unwrap();
        std::fs::create_dir_all(root.join("saves")).unwrap();

        repo.clear_shared_assets("test").unwrap();
        assert!(!repo.assets_dir().exists());
        assert!(repo.libraries_dir().exists());
        repo.clear_shared_libraries().unwrap();
        assert!(!repo.libraries_dir().exists());
        repo.clean_instance_logs("test").unwrap();
        assert!(!root.join("logs").exists());
        assert!(!root.join("crash-reports").exists());
        assert!(root.join("saves").exists());
    }

    #[test]
    fn manifest_entries_parse_type_and_release_time_including_underscore_forms() {
        let manifest: VersionManifest = serde_json::from_str(
            r#"{"versions": [
                {"id": "1.20.1", "type": "release", "url": "https://example/1.20.1.json", "time": "2023-06-12T13:25:51+00:00", "releaseTime": "2023-06-07T11:29:11+00:00"},
                {"id": "23w31a", "type": "snapshot", "url": "https://example/23w31a.json", "releaseTime": "2023-08-02T10:00:00+00:00"},
                {"id": "b1.7.3", "type": "old_beta", "url": "https://example/b1.7.3.json", "releaseTime": "2011-07-08T10:07:00+00:00"},
                {"id": "a1.0.4", "type": "old_alpha", "url": "https://example/a1.0.4.json", "releaseTime": "2010-11-30T16:13:00+00:00"}
            ]}"#,
        )
        .unwrap();

        use crate::version::ReleaseType;
        let types: Vec<_> = manifest.versions.iter().map(|v| v.release_type).collect();
        assert_eq!(
            types,
            vec![
                Some(ReleaseType::Release),
                Some(ReleaseType::Snapshot),
                Some(ReleaseType::OldBeta),
                Some(ReleaseType::OldAlpha)
            ],
            "old_beta/old_alpha 用的是下划线, 不能被吞成 Unknown"
        );

        let release = manifest.find("1.20.1").unwrap();
        assert_eq!(
            release.release_date_parts(),
            Some(("2023", "06", "07", "11", "29", "11"))
        );
    }

    #[test]
    fn manifest_entry_without_optional_fields_still_parses() {
        let manifest: VersionManifest =
            serde_json::from_str(r#"{"versions": [{"id": "x", "url": "https://example/x.json"}]}"#)
                .unwrap();
        let entry = manifest.find("x").unwrap();
        assert_eq!(entry.release_type, None);
        assert_eq!(
            entry.release_date_parts(),
            None,
            "没有 releaseTime 时不该 panic, 给 None"
        );
    }
}
