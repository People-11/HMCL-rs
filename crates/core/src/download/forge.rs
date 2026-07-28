use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use zip::ZipArchive;

use crate::download::{fetch_to_file, CacheRepository, DownloadProvider, Expected, FetchError};
use crate::install::{self, GameRepository};
use crate::version::{Artifact, DownloadType, Env, Library, Version, VersionError};

pub const PATCH_ID: &str = "forge";

#[derive(Debug, thiserror::Error)]
pub enum ForgeInstallError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("failed to parse forge installer json: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Artifact(#[from] VersionError),
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error("malformed forge install profile: {0}")]
    MalformedInstaller(String),
    #[error("illegal pattern (bad escape or unclosed bracket): {0}")]
    BadPattern(String),
    #[error("illegal pattern {0}: missing key {1}")]
    MissingKey(String, String),
    #[error("jar has no Main-Class in its manifest: {0}")]
    MissingMainClass(PathBuf),
    #[error("processor dependency missing (should have been downloaded/copied already): {0}")]
    MissingProcessorDependency(PathBuf),
    #[error("processor output file missing after execution: {0}")]
    MissingOutput(PathBuf),
    #[error("processor {jar} exited with code {code}")]
    ProcessorFailed { jar: String, code: i32 },
    #[error(
        "processor output checksum mismatch for {path:?}: expected sha1 {expected}, got {actual}"
    )]
    ChecksumMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstallProfile {
    pub json: String,
    #[serde(default)]
    pub path: Option<Artifact>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(default)]
    pub processors: Vec<Processor>,
    #[serde(default)]
    pub data: HashMap<String, Datum>,
}

impl InstallProfile {
    pub fn client_processors(&self) -> impl Iterator<Item = &Processor> {
        self.processors.iter().filter(|p| p.is_side("client"))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Processor {
    #[serde(default)]
    pub sides: Option<Vec<String>>,
    pub jar: Artifact,
    #[serde(default)]
    pub classpath: Vec<Artifact>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub outputs: HashMap<String, String>,
}

impl Processor {
    pub fn is_side(&self, side: &str) -> bool {
        self.sides
            .as_ref()
            .map(|sides| sides.iter().any(|s| s == side))
            .unwrap_or(true)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Datum {
    pub client: String,
}

fn strip_surrounding<'a>(s: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() + suffix.len() && s.starts_with(prefix) && s.ends_with(suffix) {
        Some(&s[prefix.len()..s.len() - suffix.len()])
    } else {
        None
    }
}

fn replace_tokens(
    tokens: &HashMap<String, String>,
    value: &str,
) -> Result<String, ForgeInstallError> {
    let v: Vec<char> = value.chars().collect();
    let mut buf = String::new();
    let mut x = 0usize;
    while x < v.len() {
        let c = v[x];
        if c == '\\' {
            if x == v.len() - 1 {
                return Err(ForgeInstallError::BadPattern(value.to_string()));
            }
            x += 1;
            buf.push(v[x]);
        } else if c == '{' || c == '\'' {
            let mut key = String::new();
            let mut y = x + 1;
            let mut closed = false;
            while y < v.len() {
                let d = v[y];
                if d == '\\' {
                    if y == v.len() - 1 {
                        return Err(ForgeInstallError::BadPattern(value.to_string()));
                    }
                    y += 1;
                    key.push(v[y]);
                } else if (c == '{' && d == '}') || (c == '\'' && d == '\'') {
                    x = y;
                    closed = true;
                    break;
                } else {
                    key.push(d);
                }
                y += 1;
            }
            if !closed {
                return Err(ForgeInstallError::BadPattern(value.to_string()));
            }
            if c == '\'' {
                buf.push_str(&key);
            } else {
                match tokens.get(&key) {
                    Some(val) => buf.push_str(val),
                    None => return Err(ForgeInstallError::MissingKey(value.to_string(), key)),
                }
            }
        } else {
            buf.push(c);
        }
        x += 1;
    }
    Ok(buf)
}

fn parse_literal<F>(
    literal: &str,
    vars: &HashMap<String, String>,
    repo: &GameRepository,
    plain: F,
) -> Result<Option<String>, ForgeInstallError>
where
    F: FnOnce(String) -> Result<String, ForgeInstallError>,
{
    if let Some(key) = strip_surrounding(literal, "{", "}") {
        return Ok(vars.get(key).cloned());
    }
    if let Some(inner) = strip_surrounding(literal, "'", "'") {
        return Ok(Some(inner.to_string()));
    }
    if let Some(desc) = strip_surrounding(literal, "[", "]") {
        let artifact = Artifact::from_descriptor(desc)?;
        return Ok(Some(
            repo.artifact_file(&artifact).to_string_lossy().into_owned(),
        ));
    }
    let replaced = replace_tokens(vars, literal)?;
    Ok(Some(plain(replaced)?))
}

fn parse_literal_identity(
    literal: &str,
    vars: &HashMap<String, String>,
    repo: &GameRepository,
) -> Result<Option<String>, ForgeInstallError> {
    parse_literal(literal, vars, repo, Ok)
}

fn parse_options(
    args: &[String],
    vars: &HashMap<String, String>,
    repo: &GameRepository,
) -> Result<HashMap<String, String>, ForgeInstallError> {
    let mut options = HashMap::new();
    let mut option_name: Option<String> = None;
    for arg in args {
        if let Some(name) = arg.strip_prefix("--") {
            if let Some(prev) = option_name.take() {
                options.insert(prev, String::new());
            }
            option_name = Some(name.to_string());
        } else if let Some(name) = option_name.take() {
            let parsed = parse_literal_identity(arg, vars, repo)?.unwrap_or_default();
            options.insert(name, parsed);
        }
    }
    if let Some(name) = option_name {
        options.insert(name, String::new());
    }
    Ok(options)
}

/// `pub(super)`：[`super::forge_old`] 复用这个 zip 读取小工具（老版本安装器也是
/// 同样的"打开 jar、读 install_profile.json、按需拷贝内嵌文件"套路，没必要另写一份）。
pub(super) struct InstallerArchive {
    archive: ZipArchive<std::fs::File>,
    extract_counter: u32,
}

/// Java 那边是拿一个真正的 `java.nio` ZIP 文件系统来当路径解析（`fs.getPath("/data/x")`），
/// 前导 `/` 会被当成"这个文件系统的根"正常处理。`zip` crate 没有这层文件系统抽象，
/// entry 名字存的时候就没有前导斜杠——所有从 install_profile.json 里拿到的路径
/// (`json`/`data` 里的裸文件引用) 在拿去查 zip 条目之前都要过一遍这个函数。
fn zip_entry_name(name: &str) -> &str {
    name.trim_start_matches('/')
}

impl InstallerArchive {
    pub(super) fn open(path: &Path) -> Result<InstallerArchive, ForgeInstallError> {
        let file = std::fs::File::open(path)?;
        Ok(InstallerArchive {
            archive: ZipArchive::new(file)?,
            extract_counter: 0,
        })
    }

    pub(super) fn read_json<T: serde::de::DeserializeOwned>(
        &mut self,
        name: &str,
    ) -> Result<T, ForgeInstallError> {
        let mut entry = self.archive.by_name(zip_entry_name(name))?;
        let mut text = String::new();
        entry.read_to_string(&mut text)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub(super) fn copy_to(&mut self, name: &str, dest: &Path) -> Result<bool, ForgeInstallError> {
        let mut entry = match self.archive.by_name(zip_entry_name(name)) {
            Ok(e) => e,
            Err(zip::result::ZipError::FileNotFound) => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(dest)?;
        std::io::copy(&mut entry, &mut out)?;
        Ok(true)
    }

    fn extract_to_temp(
        &mut self,
        name: &str,
        temp_dir: &Path,
    ) -> Result<String, ForgeInstallError> {
        std::fs::create_dir_all(temp_dir)?;
        self.extract_counter += 1;
        let file_name = Path::new(name)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("data");
        let dest = temp_dir.join(format!("{}-{file_name}", self.extract_counter));
        let mut entry = self.archive.by_name(zip_entry_name(name)).map_err(|_| {
            ForgeInstallError::MalformedInstaller(format!("data 条目引用的包内文件不存在: {name}"))
        })?;
        let mut out = std::fs::File::create(&dest)?;
        std::io::copy(&mut entry, &mut out)?;
        Ok(dest.to_string_lossy().into_owned())
    }
}

fn read_jar_main_class(jar_path: &Path) -> Result<String, ForgeInstallError> {
    let file = std::fs::File::open(jar_path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut manifest = String::new();
    archive
        .by_name("META-INF/MANIFEST.MF")?
        .read_to_string(&mut manifest)?;

    let normalized = manifest.replace("\r\n", "\n");
    let mut logical_lines: Vec<String> = Vec::new();
    for line in normalized.split('\n') {
        if let Some(rest) = line.strip_prefix(' ') {
            if let Some(last) = logical_lines.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        logical_lines.push(line.to_string());
    }

    logical_lines
        .iter()
        .find_map(|l| l.strip_prefix("Main-Class:").map(|v| v.trim().to_string()))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ForgeInstallError::MissingMainClass(jar_path.to_path_buf()))
}

fn sha1_hex_file(path: &Path) -> std::io::Result<String> {
    Ok(crate::download::fetch::sha1_hex(&std::fs::read(path)?))
}

async fn run_processor(
    processor: &Processor,
    vars: &HashMap<String, String>,
    repo: &GameRepository,
    java_binary: &Path,
) -> Result<(), ForgeInstallError> {
    let mut outputs = HashMap::new();
    let mut miss = false;

    for (key, value) in &processor.outputs {
        let key = parse_literal_identity(key, vars, repo)?.ok_or_else(|| {
            ForgeInstallError::MalformedInstaller(format!("processor output key 无法解析: {key}"))
        })?;
        let value = parse_literal_identity(value, vars, repo)?.ok_or_else(|| {
            ForgeInstallError::MalformedInstaller(format!(
                "processor output value 无法解析: {value}"
            ))
        })?;

        let path = PathBuf::from(&key);
        if path.is_file() {
            let code = sha1_hex_file(&path)?;
            if !code.eq_ignore_ascii_case(&value) {
                std::fs::remove_file(&path)?;
                tracing::info!(path = %path.display(), "found existing forge processor output but its checksum is stale");
                miss = true;
            }
        } else {
            miss = true;
        }
        outputs.insert(key, value);
    }

    if !processor.outputs.is_empty() && !miss {
        return Ok(()); // 所有输出都已经就绪且校验通过, 不用重跑这个 processor
    }

    let jar_path = repo.artifact_file(&processor.jar);
    if !jar_path.is_file() {
        return Err(ForgeInstallError::MissingProcessorDependency(jar_path));
    }

    let main_class = read_jar_main_class(&jar_path)?;

    let mut classpath = Vec::with_capacity(processor.classpath.len() + 1);
    for artifact in &processor.classpath {
        let file = repo.artifact_file(artifact);
        if !file.is_file() {
            return Err(ForgeInstallError::MissingProcessorDependency(file));
        }
        classpath.push(file.to_string_lossy().into_owned());
    }
    classpath.push(jar_path.to_string_lossy().into_owned());

    let mut args = Vec::with_capacity(processor.args.len());
    for arg in &processor.args {
        let parsed = parse_literal_identity(arg, vars, repo)?.ok_or_else(|| {
            ForgeInstallError::MalformedInstaller(format!("processor arg 无法解析: {arg}"))
        })?;
        args.push(parsed);
    }

    let mut command = tokio::process::Command::new(java_binary);
    crate::platform::hide_console_window(&mut command);
    command
        .arg("-cp")
        .arg(classpath.join(";"))
        .arg(&main_class)
        .args(&args);

    tracing::info!(jar = %processor.jar, mainClass = %main_class, ?args, "executing forge install processor");
    let status = command.status().await?;
    if !status.success() {
        return Err(ForgeInstallError::ProcessorFailed {
            jar: processor.jar.to_string(),
            code: status.code().unwrap_or(-1),
        });
    }

    for (path_str, expected_sha1) in &outputs {
        let path = PathBuf::from(path_str);
        if !path.is_file() {
            return Err(ForgeInstallError::MissingOutput(path));
        }
        let code = sha1_hex_file(&path)?;
        if !code.eq_ignore_ascii_case(expected_sha1) {
            std::fs::remove_file(&path)?;
            return Err(ForgeInstallError::ChecksumMismatch {
                path,
                expected: expected_sha1.clone(),
                actual: code,
            });
        }
    }

    Ok(())
}

async fn maybe_download_mojmaps(
    processor: &Processor,
    vars: &HashMap<String, String>,
    repo: &GameRepository,
    vanilla_version: &Version,
    client: &reqwest::Client,
    provider: &DownloadProvider,
) -> Result<bool, ForgeInstallError> {
    let options = parse_options(&processor.args, vars, repo)?;
    if options.get("task").map(String::as_str) != Some("DOWNLOAD_MOJMAPS")
        || options.get("side").map(String::as_str) != Some("client")
    {
        return Ok(false);
    }
    let Some(output) = options.get("output") else {
        return Ok(false);
    };

    tracing::info!(
        "patching DOWNLOAD_MOJMAPS processor with a direct Mojang client_mappings download"
    );
    let mappings = vanilla_version
        .downloads
        .as_ref()
        .and_then(|d| d.get(&DownloadType::ClientMappings))
        .ok_or_else(|| {
            ForgeInstallError::MalformedInstaller(
                "client_mappings download info not found on the vanilla version".to_string(),
            )
        })?;
    let url = mappings.url.clone().ok_or_else(|| {
        ForgeInstallError::MalformedInstaller("client_mappings has no url".to_string())
    })?;
    let expected = Expected {
        sha1: mappings.checksum().map(|s| s.to_string()),
        size: if mappings.size > 0 {
            Some(mappings.size)
        } else {
            None
        },
    };
    fetch_to_file(
        client,
        &provider.inject_url_candidates(&url),
        Path::new(output),
        &expected,
    )
    .await?;
    Ok(true)
}

/// 运行 Forge 新版（1.13+，`install_profile.json` + processors 那种）安装器,
/// 产出一个可以直接放进 `Version::patches` 的、`priority = PRIORITY_LOADER` 的 patch。
///
/// `vanilla_version` 必须是已经下载完 client.jar（`repo.version_jar(&vanilla_version.id)`
/// 在磁盘上真实存在）的原版版本——大多数 processor 都要读它，Forge 不像
/// Fabric/Quilt 那样能在完全没碰过文件系统的情况下拼出 patch。
///
/// `patch_id` 参数化是因为 [`super::neoforge`] 要复用这整套引擎：Java 版
/// `NeoForgeOldInstallTask.java` 除了类名和最后打的 patch id（"neoforge" 而不是
/// "forge"）之外，跟 `ForgeNewInstallTask.java` 是逐行相同的代码，没有必要在
/// Rust 里也照着复制粘贴一遍。
#[allow(clippy::too_many_arguments)]
pub async fn install_new_forge(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    installer_jar: &Path,
    vanilla_version: &Version,
    java_binary: &Path,
    patch_id: &str,
    self_version: &str,
) -> Result<Version, ForgeInstallError> {
    let mut installer = InstallerArchive::open(installer_jar)?;
    let profile: InstallProfile = installer.read_json("install_profile.json")?;
    let mut forge_version: Version = installer.read_json(&profile.json)?;

    let env = Env::current("");

    for lib in &profile.libraries {
        let dest = repo.library_file(lib, env);
        installer.copy_to(&format!("maven/{}", lib.path(env)), &dest)?;
    }
    if let Some(path_artifact) = &profile.path {
        let dest = repo.artifact_file(path_artifact);
        installer.copy_to(&format!("maven/{}", path_artifact.path()), &dest)?;
    }

    let mut lib_carrier = Version::new(format!("{patch_id}-processor-deps"));
    lib_carrier.libraries = profile.libraries.clone();
    install::install_libraries(client, provider, cache, repo, &lib_carrier, env).await;

    let temp_dir =
        std::env::temp_dir().join(format!("hmcl-rs-forge-installer-{}", std::process::id()));
    let mut vars = HashMap::new();
    for (key, datum) in &profile.data {
        let value = parse_literal(&datum.client, &HashMap::new(), repo, |raw| {
            installer.extract_to_temp(&raw, &temp_dir)
        })?
        .ok_or_else(|| {
            ForgeInstallError::MalformedInstaller(format!("data 条目解析出了空值: {key}"))
        })?;
        vars.insert(key.clone(), value);
    }

    vars.insert("SIDE".to_string(), "client".to_string());
    let minecraft_jar = repo
        .version_jar(&vanilla_version.id)
        .to_string_lossy()
        .into_owned();
    vars.insert("MINECRAFT_JAR".to_string(), minecraft_jar.clone());
    vars.insert("MINECRAFT_VERSION".to_string(), minecraft_jar);
    vars.insert("ROOT".to_string(), repo.root.to_string_lossy().into_owned());
    vars.insert(
        "INSTALLER".to_string(),
        installer_jar.to_string_lossy().into_owned(),
    );
    vars.insert(
        "LIBRARY_DIR".to_string(),
        repo.libraries_dir().to_string_lossy().into_owned(),
    );

    let mut done = 0usize;
    let total = profile.client_processors().count();
    for processor in profile.client_processors() {
        if maybe_download_mojmaps(processor, &vars, repo, vanilla_version, client, provider).await?
        {
            done += 1;
            tracing::info!(
                done,
                total,
                "forge install processor (DOWNLOAD_MOJMAPS patch)"
            );
            continue;
        }
        run_processor(processor, &vars, repo, java_binary).await?;
        done += 1;
        tracing::info!(done, total, jar = %processor.jar, "forge install processor finished");
    }

    install::install_libraries(client, provider, cache, repo, &forge_version, env).await;

    let _ = std::fs::remove_dir_all(&temp_dir);

    forge_version.priority = Some(Version::PRIORITY_LOADER);
    forge_version.id = patch_id.to_string();
    forge_version.version = Some(self_version.to_string());
    Ok(forge_version)
}

#[derive(Debug, Clone, Deserialize)]
struct BmclForgeFile {
    format: String,
    category: String,
    /// installer jar 的 sha1。BMCLAPI 给的这个哈希跟 Forge 官方 maven 上的同一个
    /// 文件是对得上的（实测 1.20.1-47.4.22 一致），所以哪怕我们从官方源下载也能
    /// 拿它来校验。老构建可能没这个字段，因此是 `Option`。
    #[serde(default)]
    hash: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BmclForgeVersion {
    #[serde(default)]
    branch: Option<String>,
    build: i64,
    #[serde(rename = "mcversion")]
    mc_version: String,
    version: String,
    files: Vec<BmclForgeFile>,
}

#[derive(Debug, Clone)]
pub struct ForgeBuild {
    pub mc_version: String,
    pub version: String,
    pub build: i64,
    pub installer_url: String,
    /// installer jar 的 sha1（来自 BMCLAPI 的构建列表），下载后用它校验。
    pub installer_sha1: Option<String>,
}

pub async fn fetch_compatible_builds(
    client: &reqwest::Client,
    api_root: &str,
    game_version: &str,
) -> Result<Vec<ForgeBuild>, ForgeInstallError> {
    let url = format!("{api_root}/forge/minecraft/{game_version}");
    let text = client
        .get(&url)
        .send()
        .await
        .map_err(FetchError::Http)?
        .error_for_status()
        .map_err(FetchError::Http)?
        .text()
        .await
        .map_err(FetchError::Http)?;
    let raw: Vec<BmclForgeVersion> = serde_json::from_str(&text)?;

    let mut builds: Vec<ForgeBuild> = raw
        .into_iter()
        .filter_map(|v| {
            let installer = v
                .files
                .iter()
                .find(|f| f.category == "installer" && f.format == "jar")?;
            let classifier = match v.branch.as_deref() {
                Some(branch) if !branch.is_empty() => format!("{}-{}-{}", v.mc_version, v.version, branch),
                _ => format!("{}-{}", v.mc_version, v.version),
            };
            let installer_url = format!("https://files.minecraftforge.net/maven/net/minecraftforge/forge/{classifier}/forge-{classifier}-installer.jar");
            Some(ForgeBuild { mc_version: v.mc_version, version: v.version, build: v.build, installer_url, installer_sha1: installer.hash.clone() })
        })
        .collect();
    builds.sort_by_key(|b| b.build);
    Ok(builds)
}

pub async fn fetch_latest_build(
    client: &reqwest::Client,
    api_root: &str,
    game_version: &str,
) -> Result<ForgeBuild, ForgeInstallError> {
    fetch_compatible_builds(client, api_root, game_version)
        .await?
        .pop()
        .ok_or_else(|| {
            ForgeInstallError::MalformedInstaller(format!(
                "no forge build available for {game_version}"
            ))
        })
}

pub async fn fetch_build_by_version(
    client: &reqwest::Client,
    api_root: &str,
    game_version: &str,
    version: &str,
) -> Result<ForgeBuild, ForgeInstallError> {
    fetch_compatible_builds(client, api_root, game_version)
        .await?
        .into_iter()
        .find(|b| b.version == version)
        .ok_or_else(|| {
            ForgeInstallError::MalformedInstaller(format!(
                "no forge build {version} for {game_version}"
            ))
        })
}

pub async fn download_installer(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    build: &ForgeBuild,
    dest: &Path,
) -> Result<(), ForgeInstallError> {
    let expected = match &build.installer_sha1 {
        Some(sha1) => Expected::sha1(sha1.clone()),
        None => Expected::default(),
    };
    fetch_to_file(
        client,
        &provider.inject_url_candidates(&build.installer_url),
        dest,
        &expected,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SAMPLE_BMCL_FORGE: &str = r#"[
        {"build": 47000001, "mcversion": "1.20.1", "version": "47.0.1", "modified": "2023-06-12T19:37:00.000Z",
         "files": [{"format": "jar", "category": "installer"}, {"format": "zip", "category": "mdk"}]},
        {"build": 47040022, "mcversion": "1.20.1", "version": "47.4.22", "modified": "2026-07-21T11:35:10.000Z",
         "files": [{"format": "txt", "category": "changelog"}, {"format": "jar", "category": "installer"}, {"format": "zip", "category": "mdk"}]},
        {"build": 47040023, "mcversion": "1.20.1", "version": "47.4.23-mdkonly", "modified": "2026-07-22T00:00:00.000Z",
         "files": [{"format": "zip", "category": "mdk"}]}
    ]"#;

    #[tokio::test]
    async fn fetch_compatible_builds_sorts_by_build_and_skips_installer_less_entries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/forge/minecraft/1.20.1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_BMCL_FORGE))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let builds = fetch_compatible_builds(&client, &server.uri(), "1.20.1")
            .await
            .unwrap();

        assert_eq!(
            builds.len(),
            2,
            "the mdk-only build (no installer) must be skipped"
        );
        assert_eq!(
            builds[0].version, "47.0.1",
            "must be sorted ascending by build number"
        );
        assert_eq!(builds[1].version, "47.4.22");
        assert_eq!(builds[1].installer_url, "https://files.minecraftforge.net/maven/net/minecraftforge/forge/1.20.1-47.4.22/forge-1.20.1-47.4.22-installer.jar");
    }

    #[tokio::test]
    async fn fetch_latest_build_picks_the_highest_build_number() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/forge/minecraft/1.20.1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_BMCL_FORGE))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let latest = fetch_latest_build(&client, &server.uri(), "1.20.1")
            .await
            .unwrap();
        assert_eq!(latest.version, "47.4.22");
    }

    #[test]
    fn replace_tokens_substitutes_braces_and_keeps_literal_escapes() {
        let mut tokens = HashMap::new();
        tokens.insert("FOO".to_string(), "bar".to_string());
        assert_eq!(
            replace_tokens(&tokens, "prefix-{FOO}-suffix").unwrap(),
            "prefix-bar-suffix"
        );
        assert_eq!(
            replace_tokens(&tokens, r"literal \{ brace").unwrap(),
            "literal { brace"
        );
        assert!(replace_tokens(&tokens, "{MISSING}").is_err());
        assert!(replace_tokens(&tokens, "unclosed {FOO").is_err());
    }

    #[test]
    fn parse_literal_dispatches_on_bracket_style() {
        let dir = std::env::temp_dir().join("hmcl-rs-test-forge-parse-literal");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = GameRepository::new(&dir);

        let mut vars = HashMap::new();
        vars.insert("MC_VERSION".to_string(), "1.20.1".to_string());

        assert_eq!(
            parse_literal_identity("{MC_VERSION}", &vars, &repo).unwrap(),
            Some("1.20.1".to_string())
        );
        assert_eq!(
            parse_literal_identity("{MISSING}", &vars, &repo).unwrap(),
            None,
            "missing var lookups resolve to None, matching Java's null"
        );
        assert_eq!(
            parse_literal_identity("'a literal string'", &vars, &repo).unwrap(),
            Some("a literal string".to_string())
        );

        let artifact_path =
            parse_literal_identity("[net.minecraftforge:forge:1.0:client]", &vars, &repo)
                .unwrap()
                .unwrap();
        assert!(
            artifact_path.ends_with("forge-1.0-client.jar"),
            "got {artifact_path}"
        );
        assert!(
            artifact_path.contains("net"),
            "must resolve under the libraries directory: {artifact_path}"
        );

        assert_eq!(
            parse_literal_identity("plain-{MC_VERSION}-text", &vars, &repo).unwrap(),
            Some("plain-1.20.1-text".to_string())
        );
    }

    #[test]
    fn parse_options_reads_flag_value_pairs() {
        let dir = std::env::temp_dir().join("hmcl-rs-test-forge-parse-options");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = GameRepository::new(&dir);
        let vars = HashMap::new();

        let args = vec![
            "--task".to_string(),
            "DOWNLOAD_MOJMAPS".to_string(),
            "--side".to_string(),
            "client".to_string(),
            "--output".to_string(),
            "'/tmp/out.txt'".to_string(),
        ];
        let options = parse_options(&args, &vars, &repo).unwrap();
        assert_eq!(
            options.get("task").map(String::as_str),
            Some("DOWNLOAD_MOJMAPS")
        );
        assert_eq!(options.get("side").map(String::as_str), Some("client"));
        assert_eq!(
            options.get("output").map(String::as_str),
            Some("/tmp/out.txt")
        );
    }

    fn write_test_jar(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        for (name, content) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn reads_main_class_from_manifest_with_continuation_line() {
        let dir = std::env::temp_dir().join("hmcl-rs-test-forge-manifest");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jar_path = dir.join("processor.jar");

        let manifest =
            b"Manifest-Version: 1.0\r\nMain-Class: com.example.ver\r\n yLongPackage.Main\r\n";
        write_test_jar(&jar_path, &[("META-INF/MANIFEST.MF", manifest)]);

        let main_class = read_jar_main_class(&jar_path).unwrap();
        assert_eq!(main_class, "com.example.veryLongPackage.Main");
    }

    #[test]
    fn install_profile_json_shape_matches_real_forge_installers() {
        const SAMPLE: &str = r#"{
            "spec": 0,
            "profile": "forge",
            "version": "1.20.1-forge-47.2.0",
            "minecraft": "1.20.1",
            "json": "/version.json",
            "path": "net.minecraftforge:forge:1.20.1-47.2.0:client",
            "libraries": [
                {"name": "net.minecraftforge:installertools:1.3.0", "downloads": {"artifact": {"path": "net/minecraftforge/installertools/1.3.0/installertools-1.3.0.jar", "url": "https://maven.minecraftforge.net/net/minecraftforge/installertools/1.3.0/installertools-1.3.0.jar", "sha1": "deadbeef", "size": 1}}}
            ],
            "processors": [
                {
                    "sides": ["client"],
                    "jar": "net.minecraftforge:installertools:1.3.0",
                    "classpath": [],
                    "args": ["--task", "MCP_DATA", "--input", "{MAPPINGS}", "--output", "{MCP_DATA_OUTPUT}"],
                    "outputs": {"{MCP_DATA_OUTPUT}": "{MCP_DATA_OUTPUT_SHA}"}
                }
            ],
            "data": {
                "MAPPINGS": {"client": "'/data/client.tsrg'"},
                "BINPATCH": {"client": "[net.minecraftforge:forge:1.20.1-47.2.0:clientdata@lzma]"}
            }
        }"#;

        let profile: InstallProfile = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(profile.json, "/version.json");
        assert_eq!(
            profile.path.as_ref().unwrap().classifier.as_deref(),
            Some("client")
        );
        assert_eq!(profile.libraries.len(), 1);
        assert_eq!(profile.client_processors().count(), 1);
        assert_eq!(
            profile.data.get("MAPPINGS").unwrap().client,
            "'/data/client.tsrg'"
        );
        assert_eq!(
            profile.data.get("BINPATCH").unwrap().client,
            "[net.minecraftforge:forge:1.20.1-47.2.0:clientdata@lzma]"
        );
    }

    #[test]
    fn processor_side_filtering_matches_java_semantics() {
        let no_sides: Processor =
            serde_json::from_str(r#"{"jar": "a:b:1.0", "outputs": {}}"#).unwrap();
        assert!(no_sides.is_side("client"));
        assert!(no_sides.is_side("server"));

        let client_only: Processor =
            serde_json::from_str(r#"{"jar": "a:b:1.0", "sides": ["client"], "outputs": {}}"#)
                .unwrap();
        assert!(client_only.is_side("client"));
        assert!(!client_only.is_side("server"));
    }
}
