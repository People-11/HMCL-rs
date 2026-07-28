use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use zip::ZipArchive;

use crate::install::GameRepository;
use crate::version::{Argument, Arguments, Artifact, Env, Library, Version};

pub const PATCH_ID: &str = "optifine";
const PRIORITY_OPTIFINE: i32 = 10000;

const VANILLA_MAIN: &str = "net.minecraft.client.main.Main";
const LAUNCH_WRAPPER_MAIN: &str = "net.minecraft.launchwrapper.Launch";
const MOD_LAUNCHER_MAIN: &str = "cpw.mods.modlauncher.Launcher";
const BOOTSTRAP_LAUNCHER_MAIN: &str = "cpw.mods.bootstraplauncher.BootstrapLauncher";
const FORGE_BOOTSTRAP_MAIN: &str = "net.minecraftforge.bootstrap.ForgeBootstrap";
const NEO_FORGE_BOOTSTRAP_MAIN: &str = "net.neoforged.fml.startup.Client";

const FORGE_OPTIFINE_MAIN: [&str; 6] = [
    VANILLA_MAIN,
    LAUNCH_WRAPPER_MAIN,
    MOD_LAUNCHER_MAIN,
    BOOTSTRAP_LAUNCHER_MAIN,
    FORGE_BOOTSTRAP_MAIN,
    NEO_FORGE_BOOTSTRAP_MAIN,
];

/// 对应 Java `LibraryAnalyzer.FORGE_OPTIFINE_BROKEN_RANGE` 的下界: OptiFine H1
/// Pre2 之前的构建不兼容现代 Forge 的 BootstrapLauncher。`buildof.txt` 里的版本号
/// 是定长的 `YYYYMMDD-HHMMSS` 格式，字符串字典序比较等价于时间先后比较，不需要
/// 为这一个用途专门写一个通用版本号比较器。
const MIN_BUILDOF_FOR_BOOTSTRAP_LAUNCHER: &str = "20210924-190833";

#[derive(Debug, thiserror::Error)]
pub enum OptiFineInstallError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("unrecognized optifine installer: no Config.class found (or it has no usable constant pool)")]
    UnrecognizedInstaller,
    #[error(
        "unrecognized optifine installer: Config.class is missing MC_VERSION/OF_EDITION/OF_RELEASE"
    )]
    MalformedMetadata,
    #[error("this optifine installer targets Minecraft {expected}, but {actual} was requested")]
    VersionMismatch { expected: String, actual: String },
    #[error("optifine cannot be installed on top of mainClass {0:?}")]
    UnsupportedMainClass(String),
    #[error("optifine's bundled Patcher exited with code {0}")]
    PatcherFailed(i32),
    #[error("this optifine build (buildof {buildof}) is too old for modern Forge's BootstrapLauncher (needs >= {MIN_BUILDOF_FOR_BOOTSTRAP_LAUNCHER})")]
    IncompatibleWithBootstrapLauncher { buildof: String },
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("failed to parse optifine version list: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no optifine build available for {0}")]
    NoBuildForGameVersion(String),
    #[error(transparent)]
    Fetch(#[from] crate::download::FetchError),
}

// ============================================================================
// class 文件常量池: 只抠 UTF8 常量, 其它 tag 原样跳过对应字节数。
// ============================================================================

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn u1(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn u2(&mut self) -> Option<u16> {
        Some(((self.u1()? as u16) << 8) | self.u1()? as u16)
    }

    fn u4(&mut self) -> Option<u32> {
        Some(((self.u2()? as u32) << 16) | self.u2()? as u32)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        if self.pos + n > self.data.len() {
            return None;
        }
        self.pos += n;
        Some(())
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return None;
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }
}

fn read_utf8_constants(data: &[u8]) -> Option<Vec<String>> {
    let mut c = Cursor { data, pos: 0 };
    if c.u4()? != 0xCAFE_BABE {
        return None;
    }
    c.skip(4)?; // minor_version + major_version
    let count = c.u2()?;

    let mut utf8s = Vec::new();
    let mut i = 1u32;
    while i < count as u32 {
        match c.u1()? {
            1 => {
                let len = c.u2()? as usize;
                utf8s.push(String::from_utf8_lossy(c.take(len)?).into_owned());
            }
            3 | 4 => c.skip(4)?, // Integer, Float
            5 | 6 => {
                c.skip(8)?; // Long, Double
                i += 1;
            }
            7 | 8 | 16 | 19 | 20 => c.skip(2)?, // Class, String, MethodType, Module, Package
            9 | 10 | 11 | 12 | 17 | 18 => c.skip(4)?, // *ref, NameAndType, Dynamic, InvokeDynamic
            15 => c.skip(3)?,                   // MethodHandle: u1 + u2
            _ => return None,
        }
        i += 1;
    }
    Some(utf8s)
}

fn constant_after<'a>(constants: &'a [String], key: &str) -> Option<&'a str> {
    let idx = constants.iter().position(|s| s == key)?;
    constants.get(idx + 1).map(String::as_str)
}

const CONFIG_CLASS_CANDIDATES: [&str; 3] = [
    "Config.class",
    "net/optifine/Config.class",
    "notch/net/optifine/Config.class",
];

fn detect_metadata(installer_jar: &Path) -> Result<(String, String, String), OptiFineInstallError> {
    let mut archive = ZipArchive::new(std::fs::File::open(installer_jar)?)?;

    let mut bytes = Vec::new();
    let found = CONFIG_CLASS_CANDIDATES
        .iter()
        .any(|name| match archive.by_name(name) {
            Ok(mut entry) => entry.read_to_end(&mut bytes).is_ok(),
            Err(_) => false,
        });
    if !found {
        return Err(OptiFineInstallError::UnrecognizedInstaller);
    }

    let constants =
        read_utf8_constants(&bytes).ok_or(OptiFineInstallError::UnrecognizedInstaller)?;
    let mc_version = constant_after(&constants, "MC_VERSION")
        .ok_or(OptiFineInstallError::MalformedMetadata)?
        .to_string();
    let of_edition = constant_after(&constants, "OF_EDITION")
        .ok_or(OptiFineInstallError::MalformedMetadata)?
        .to_string();
    let of_release = constant_after(&constants, "OF_RELEASE")
        .ok_or(OptiFineInstallError::MalformedMetadata)?
        .to_string();
    Ok((mc_version, of_edition, of_release))
}

fn strip_zip_entry(jar_path: &Path, entry_name: &str) -> Result<(), OptiFineInstallError> {
    let mut archive = ZipArchive::new(std::fs::File::open(jar_path)?)?;
    if archive.by_name(entry_name).is_err() {
        return Ok(()); // 没有这个条目, 不用碰
    }

    let tmp_path = jar_path.with_extension("tmp");
    {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&tmp_path)?);
        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            if file.name() == entry_name {
                continue;
            }
            writer.raw_copy_file(file)?;
        }
        writer.finish()?;
    }
    std::fs::rename(&tmp_path, jar_path)?;
    Ok(())
}

fn copy_zip_entry(
    installer_jar: &Path,
    entry_name: &str,
    dest: &Path,
) -> Result<bool, OptiFineInstallError> {
    let mut archive = ZipArchive::new(std::fs::File::open(installer_jar)?)?;
    let mut entry = match archive.by_name(entry_name) {
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

fn read_zip_entry_text(
    installer_jar: &Path,
    entry_name: &str,
) -> Result<Option<String>, OptiFineInstallError> {
    let mut archive = ZipArchive::new(std::fs::File::open(installer_jar)?)?;
    let result = match archive.by_name(entry_name) {
        Ok(mut entry) => {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            Ok(Some(text.trim().to_string()))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(e.into()),
    };
    result
}

async fn run_patcher(
    installer_jar: &Path,
    java_binary: &Path,
    vanilla_jar: &Path,
    optifine_lib_dest: &Path,
) -> Result<(), OptiFineInstallError> {
    let mut command = tokio::process::Command::new(java_binary);
    crate::platform::hide_console_window(&mut command);
    let status = command
        .arg("-cp")
        .arg(installer_jar)
        .arg("optifine.Patcher")
        .arg(vanilla_jar)
        .arg(installer_jar)
        .arg(optifine_lib_dest)
        .status()
        .await?;
    if !status.success() {
        return Err(OptiFineInstallError::PatcherFailed(
            status.code().unwrap_or(-1),
        ));
    }
    Ok(())
}

/// 运行 OptiFine 安装器，产出一个 `id = "optifine"`、`priority = 10000` 的 patch。
/// 按照 OptiFine 一贯"应该最后装"的惯例，这个 patch 应该被追加到
/// `Version::patches` 的末尾（而不是替换掉已有的 Forge 之类的 patch）。
///
/// `game_version_id`/`current_main_class` 是调用方已经 resolve() 过的、
/// **装 OptiFine 之前**的版本信息——分别用来核对安装器是给这个游戏版本做的、
/// 以及判断当前主类是否在 OptiFine 支持安装的范围内。
pub async fn install_optifine(
    repo: &GameRepository,
    installer_jar: &Path,
    game_version_id: &str,
    current_main_class: &str,
    java_binary: &Path,
) -> Result<Version, OptiFineInstallError> {
    let (mc_version, of_edition, of_release) = detect_metadata(installer_jar)?;
    if mc_version != game_version_id {
        return Err(OptiFineInstallError::VersionMismatch {
            expected: mc_version,
            actual: game_version_id.to_string(),
        });
    }
    if !FORGE_OPTIFINE_MAIN.contains(&current_main_class) {
        return Err(OptiFineInstallError::UnsupportedMainClass(
            current_main_class.to_string(),
        ));
    }

    let self_version = format!("{of_edition}_{of_release}");
    let maven_version = format!("{mc_version}_{self_version}");
    let env = Env::current("");

    let installer_lib = Library::from_artifact(Artifact {
        classifier: Some("installer".to_string()),
        ..Artifact::new("optifine", "OptiFine", &maven_version)
    });
    let installer_lib_dest = repo.library_file(&installer_lib, env);
    if let Some(parent) = installer_lib_dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(installer_jar, &installer_lib_dest)?;
    strip_zip_entry(&installer_lib_dest, "META-INF/mods.toml")?;

    let optifine_artifact = Artifact::new("optifine", "OptiFine", &maven_version);
    let optifine_lib = Library::from_artifact(optifine_artifact);
    let optifine_lib_dest = repo.library_file(&optifine_lib, env);
    if let Some(parent) = optifine_lib_dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let has_patcher = {
        let mut archive = ZipArchive::new(std::fs::File::open(installer_jar)?)?;
        let found = archive.by_name("optifine/Patcher.class").is_ok();
        found
    };
    if has_patcher {
        let vanilla_jar = repo.version_jar(&mc_version);
        run_patcher(installer_jar, java_binary, &vanilla_jar, &optifine_lib_dest).await?;
    } else {
        std::fs::copy(installer_jar, &optifine_lib_dest)?;
    }
    strip_zip_entry(&optifine_lib_dest, "META-INF/mods.toml")?;

    let mut libraries = vec![optifine_lib];
    let mut has_launchwrapper = false;

    if copy_zip_entry(
        installer_jar,
        "launchwrapper-2.0.jar",
        &repo.library_file(
            &Library::from_artifact(Artifact::new("optifine", "launchwrapper", "2.0")),
            env,
        ),
    )? {
        has_launchwrapper = true;
        libraries.push(Library::from_artifact(Artifact::new(
            "optifine",
            "launchwrapper",
            "2.0",
        )));
    }

    if let Some(launchwrapper_of_version) =
        read_zip_entry_text(installer_jar, "launchwrapper-of.txt")?
    {
        let jar_entry = format!("launchwrapper-of-{launchwrapper_of_version}.jar");
        let lib = Library::from_artifact(Artifact::new(
            "optifine",
            "launchwrapper-of",
            &launchwrapper_of_version,
        ));
        if copy_zip_entry(installer_jar, &jar_entry, &repo.library_file(&lib, env))? {
            has_launchwrapper = true;
            libraries.push(lib);
        }
    }

    if let Some(buildof) = read_zip_entry_text(installer_jar, "buildof.txt")? {
        if current_main_class == BOOTSTRAP_LAUNCHER_MAIN
            && buildof.as_str() < MIN_BUILDOF_FOR_BOOTSTRAP_LAUNCHER
        {
            return Err(OptiFineInstallError::IncompatibleWithBootstrapLauncher { buildof });
        }
    }

    if !has_launchwrapper {
        // 没有自带任何一种 launchwrapper 变体: 落回标准的
        // net.minecraft:launchwrapper:1.12(老 Forge/FML 用的那个)——这个库没有从
        // 安装器里抠文件, 得靠调用方之后对最终合并版本跑一遍 install_libraries
        // (跟老版本 Forge 缺依赖同一个补齐机制, 见 forge_old.rs)。
        libraries.push(Library::from_artifact(Artifact::new(
            "net.minecraft",
            "launchwrapper",
            "1.12",
        )));
    }

    let mut patch = Version::new(PATCH_ID);
    patch.priority = Some(PRIORITY_OPTIFINE);
    patch.version = Some(self_version);
    patch.main_class = Some(LAUNCH_WRAPPER_MAIN.to_string());
    patch.arguments = Some(Arguments {
        game: Some(vec![
            Argument::Plain("--tweakClass".to_string()),
            Argument::Plain("optifine.OptiFineTweaker".to_string()),
        ]),
        jvm: None,
    });
    patch.libraries = libraries;
    Ok(patch)
}

#[derive(Debug, Clone, Deserialize)]
struct BmclOptiFineVersion {
    #[serde(rename = "mcversion")]
    mc_version: String,
    #[serde(rename = "type")]
    edition: String,
    patch: String,
}

#[derive(Debug, Clone)]
pub struct OptiFineBuild {
    pub mc_version: String,
    pub version: String,
    pub download_url: String,
}

pub async fn fetch_compatible_builds(
    client: &reqwest::Client,
    api_root: &str,
    game_version: &str,
) -> Result<Vec<OptiFineBuild>, OptiFineInstallError> {
    let url = format!("{api_root}/optifine/{game_version}");
    let text = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let raw: Vec<BmclOptiFineVersion> = serde_json::from_str(&text)?;

    Ok(raw
        .into_iter()
        .map(|v| {
            let download_url = format!(
                "{api_root}/optifine/{}/{}/{}",
                v.mc_version, v.edition, v.patch
            );
            OptiFineBuild {
                mc_version: v.mc_version,
                version: format!("{}_{}", v.edition, v.patch),
                download_url,
            }
        })
        .collect())
}

pub async fn fetch_latest_build(
    client: &reqwest::Client,
    api_root: &str,
    game_version: &str,
) -> Result<OptiFineBuild, OptiFineInstallError> {
    fetch_compatible_builds(client, api_root, game_version)
        .await?
        .pop()
        .ok_or_else(|| OptiFineInstallError::NoBuildForGameVersion(game_version.to_string()))
}

pub async fn fetch_build_by_version(
    client: &reqwest::Client,
    api_root: &str,
    game_version: &str,
    version: &str,
) -> Result<OptiFineBuild, OptiFineInstallError> {
    fetch_compatible_builds(client, api_root, game_version)
        .await?
        .into_iter()
        .find(|b| b.version == version)
        .ok_or_else(|| {
            OptiFineInstallError::NoBuildForGameVersion(format!("{version} for {game_version}"))
        })
}

pub async fn download_installer(
    client: &reqwest::Client,
    build: &OptiFineBuild,
    dest: &Path,
) -> Result<(), OptiFineInstallError> {
    crate::download::fetch_to_file(
        client,
        std::slice::from_ref(&build.download_url),
        dest,
        &crate::download::Expected::default(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use wiremock::matchers::{method, path as path_matcher};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SAMPLE_BMCL_OPTIFINE: &str = r#"[
        {"mcversion": "1.20.1", "patch": "I5", "type": "HD_U", "filename": "OptiFine_1.20.1_HD_U_I5.jar", "forge": "Forge 47.0.35"},
        {"mcversion": "1.20.1", "patch": "I6", "type": "HD_U", "filename": "OptiFine_1.20.1_HD_U_I6.jar", "forge": "Forge 47.2.18"}
    ]"#;

    #[tokio::test]
    async fn fetch_compatible_builds_parses_real_shaped_response_and_builds_download_urls() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/optifine/1.20.1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_BMCL_OPTIFINE))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let builds = fetch_compatible_builds(&client, &server.uri(), "1.20.1")
            .await
            .unwrap();

        assert_eq!(builds.len(), 2);
        assert_eq!(builds[1].version, "HD_U_I6");
        assert_eq!(
            builds[1].download_url,
            format!("{}/optifine/1.20.1/HD_U/I6", server.uri())
        );

        let latest = fetch_latest_build(&client, &server.uri(), "1.20.1")
            .await
            .unwrap();
        assert_eq!(
            latest.version, "HD_U_I6",
            "latest = last entry in API order, matching real release ordering"
        );

        let specific = fetch_build_by_version(&client, &server.uri(), "1.20.1", "HD_U_I5")
            .await
            .unwrap();
        assert_eq!(
            specific.download_url,
            format!("{}/optifine/1.20.1/HD_U/I5", server.uri())
        );
    }

    fn build_class_file(utf8_constants: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0xCAFEBABEu32.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&52u16.to_be_bytes()); // major (随便填一个真实值)
        out.extend_from_slice(&((utf8_constants.len() as u16) + 1).to_be_bytes()); // constant_pool_count
        for s in utf8_constants {
            out.push(1); // CONSTANT_Utf8
            out.extend_from_slice(&(s.len() as u16).to_be_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        out
    }

    #[test]
    fn reads_utf8_constants_in_pool_order() {
        let class = build_class_file(&[
            "hello",
            "MC_VERSION",
            "1.20.1",
            "OF_EDITION",
            "HD_U",
            "OF_RELEASE",
            "I6",
        ]);
        let constants = read_utf8_constants(&class).unwrap();
        assert_eq!(
            constants,
            vec![
                "hello",
                "MC_VERSION",
                "1.20.1",
                "OF_EDITION",
                "HD_U",
                "OF_RELEASE",
                "I6"
            ]
        );
    }

    #[test]
    fn constant_after_finds_the_adjacent_value() {
        let constants: Vec<String> = ["MC_VERSION", "1.20.1", "OF_EDITION", "HD_U"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(constant_after(&constants, "MC_VERSION"), Some("1.20.1"));
        assert_eq!(constant_after(&constants, "OF_EDITION"), Some("HD_U"));
        assert_eq!(constant_after(&constants, "MISSING"), None);
    }

    #[test]
    fn rejects_data_without_the_class_file_magic() {
        assert!(read_utf8_constants(b"not a class file").is_none());
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

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test-optifine")
            .join(name)
            .join(format!("{:x}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn detects_metadata_from_a_realistic_config_class() {
        let dir = tmp_dir("detect_metadata");
        let installer_path = dir.join("OptiFine_installer.jar");
        let config_class = build_class_file(&[
            "irrelevant",
            "MC_VERSION",
            "1.20.1",
            "junk",
            "OF_EDITION",
            "HD_U",
            "OF_RELEASE",
            "I6",
            "trailing",
        ]);
        write_test_jar(
            &installer_path,
            &[("net/optifine/Config.class", &config_class)],
        );

        let (mc, edition, release) = detect_metadata(&installer_path).unwrap();
        assert_eq!(mc, "1.20.1");
        assert_eq!(edition, "HD_U");
        assert_eq!(release, "I6");
    }

    #[test]
    fn missing_config_class_is_an_unrecognized_installer() {
        let dir = tmp_dir("missing_config");
        let installer_path = dir.join("not-optifine.jar");
        write_test_jar(&installer_path, &[("README.txt", b"hi")]);
        let err = detect_metadata(&installer_path).unwrap_err();
        assert!(matches!(err, OptiFineInstallError::UnrecognizedInstaller));
    }

    #[tokio::test]
    async fn installs_a_legacy_style_installer_with_no_patcher_and_no_bundled_launchwrapper() {
        let dir = tmp_dir("legacy_install");
        let installer_path = dir.join("OptiFine_1.7.10_HD_U_D5.jar");
        let config_class = build_class_file(&[
            "MC_VERSION",
            "1.7.10",
            "OF_EDITION",
            "HD_U",
            "OF_RELEASE",
            "D5",
        ]);
        write_test_jar(
            &installer_path,
            &[
                ("Config.class", &config_class),
                ("some/other/class.class", b"whatever"),
            ],
        );

        let root = dir.join("mc");
        let repo = GameRepository::new(&root);
        let patch = install_optifine(
            &repo,
            &installer_path,
            "1.7.10",
            VANILLA_MAIN,
            Path::new("java"),
        )
        .await
        .expect("legacy optifine install without a patcher must succeed with no JVM spawn");

        assert_eq!(patch.id, PATCH_ID);
        assert_eq!(patch.version.as_deref(), Some("HD_U_D5"));
        assert_eq!(patch.priority, Some(PRIORITY_OPTIFINE));
        assert_eq!(patch.main_class.as_deref(), Some(LAUNCH_WRAPPER_MAIN));
        assert!(patch.libraries.iter().any(|l| l.is("optifine", "OptiFine")));
        assert!(patch.libraries.iter().any(|l| l.is("net.minecraft", "launchwrapper")), "no bundled launchwrapper variant found: must fall back to net.minecraft:launchwrapper:1.12");

        let optifine_jar = repo.library_file(
            &Library::from_artifact(Artifact::new("optifine", "OptiFine", "1.7.10_HD_U_D5")),
            Env::current(""),
        );
        assert_eq!(
            std::fs::read(&optifine_jar).unwrap(),
            std::fs::read(&installer_path).unwrap(),
            "without a Patcher.class, the whole installer jar becomes the runtime library verbatim"
        );
    }

    #[tokio::test]
    async fn installs_a_bundled_launchwrapper_variant_and_skips_the_1_12_fallback() {
        let dir = tmp_dir("bundled_launchwrapper");
        let installer_path = dir.join("OptiFine_1.16.5_HD_U_G8.jar");
        let config_class = build_class_file(&[
            "MC_VERSION",
            "1.16.5",
            "OF_EDITION",
            "HD_U",
            "OF_RELEASE",
            "G8",
        ]);
        write_test_jar(
            &installer_path,
            &[
                ("Config.class", &config_class),
                ("launchwrapper-2.0.jar", b"fake patched launchwrapper"),
            ],
        );

        let root = dir.join("mc");
        let repo = GameRepository::new(&root);
        let patch = install_optifine(
            &repo,
            &installer_path,
            "1.16.5",
            VANILLA_MAIN,
            Path::new("java"),
        )
        .await
        .unwrap();

        assert!(patch
            .libraries
            .iter()
            .any(|l| l.is("optifine", "launchwrapper")));
        assert!(
            !patch
                .libraries
                .iter()
                .any(|l| l.is("net.minecraft", "launchwrapper")),
            "a bundled launchwrapper must suppress the 1.12 fallback"
        );

        let lw_dest = repo.library_file(
            &Library::from_artifact(Artifact::new("optifine", "launchwrapper", "2.0")),
            Env::current(""),
        );
        assert_eq!(
            std::fs::read(&lw_dest).unwrap(),
            b"fake patched launchwrapper"
        );
    }

    #[tokio::test]
    async fn version_mismatch_is_rejected() {
        let dir = tmp_dir("version_mismatch");
        let installer_path = dir.join("OptiFine_1.20.1.jar");
        let config_class = build_class_file(&[
            "MC_VERSION",
            "1.20.1",
            "OF_EDITION",
            "HD_U",
            "OF_RELEASE",
            "I6",
        ]);
        write_test_jar(&installer_path, &[("Config.class", &config_class)]);

        let root = dir.join("mc");
        let repo = GameRepository::new(&root);
        let err = install_optifine(
            &repo,
            &installer_path,
            "1.19.4",
            VANILLA_MAIN,
            Path::new("java"),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OptiFineInstallError::VersionMismatch { .. }));
    }

    #[tokio::test]
    async fn unsupported_main_class_is_rejected() {
        let dir = tmp_dir("unsupported_main_class");
        let installer_path = dir.join("OptiFine_1.20.1.jar");
        let config_class = build_class_file(&[
            "MC_VERSION",
            "1.20.1",
            "OF_EDITION",
            "HD_U",
            "OF_RELEASE",
            "I6",
        ]);
        write_test_jar(&installer_path, &[("Config.class", &config_class)]);

        let root = dir.join("mc");
        let repo = GameRepository::new(&root);
        let err = install_optifine(
            &repo,
            &installer_path,
            "1.20.1",
            "net.fabricmc.loader.impl.launch.knot.KnotClient",
            Path::new("java"),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, OptiFineInstallError::UnsupportedMainClass(_)),
            "optifine cannot be layered on top of fabric"
        );
    }

    #[tokio::test]
    async fn old_optifine_build_is_rejected_on_modern_forge_bootstrap_launcher() {
        let dir = tmp_dir("buildof_gate");
        let installer_path = dir.join("OptiFine_1.18.2_HD_U_H1_pre1.jar");
        let config_class = build_class_file(&[
            "MC_VERSION",
            "1.18.2",
            "OF_EDITION",
            "HD_U",
            "OF_RELEASE",
            "H1_pre1",
        ]);
        write_test_jar(
            &installer_path,
            &[
                ("Config.class", &config_class),
                ("buildof.txt", b"20210101-000000"),
            ],
        );

        let root = dir.join("mc");
        let repo = GameRepository::new(&root);
        let err = install_optifine(
            &repo,
            &installer_path,
            "1.18.2",
            BOOTSTRAP_LAUNCHER_MAIN,
            Path::new("java"),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            OptiFineInstallError::IncompatibleWithBootstrapLauncher { .. }
        ));
    }

    #[tokio::test]
    async fn new_enough_optifine_build_is_accepted_on_modern_forge_bootstrap_launcher() {
        let dir = tmp_dir("buildof_gate_ok");
        let installer_path = dir.join("OptiFine_1.18.2_HD_U_H1.jar");
        let config_class = build_class_file(&[
            "MC_VERSION",
            "1.18.2",
            "OF_EDITION",
            "HD_U",
            "OF_RELEASE",
            "H1",
        ]);
        write_test_jar(
            &installer_path,
            &[
                ("Config.class", &config_class),
                ("buildof.txt", b"20211231-000000"),
            ],
        );

        let root = dir.join("mc");
        let repo = GameRepository::new(&root);
        install_optifine(
            &repo,
            &installer_path,
            "1.18.2",
            BOOTSTRAP_LAUNCHER_MAIN,
            Path::new("java"),
        )
        .await
        .expect("a new enough optifine build must be accepted");
    }

    #[test]
    fn strip_zip_entry_removes_only_the_named_entry() {
        let dir = tmp_dir("strip_entry");
        let jar_path = dir.join("test.jar");
        write_test_jar(
            &jar_path,
            &[
                ("META-INF/mods.toml", b"[[mods]]"),
                ("keep/me.txt", b"still here"),
            ],
        );

        strip_zip_entry(&jar_path, "META-INF/mods.toml").unwrap();

        let mut archive = ZipArchive::new(std::fs::File::open(&jar_path).unwrap()).unwrap();
        assert!(
            archive.by_name("META-INF/mods.toml").is_err(),
            "the stripped entry must be gone"
        );
        let mut kept = String::new();
        archive
            .by_name("keep/me.txt")
            .unwrap()
            .read_to_string(&mut kept)
            .unwrap();
        assert_eq!(
            kept, "still here",
            "other entries must survive byte-for-byte"
        );
    }

    #[test]
    fn strip_zip_entry_is_a_no_op_when_the_entry_does_not_exist() {
        let dir = tmp_dir("strip_entry_noop");
        let jar_path = dir.join("test.jar");
        write_test_jar(&jar_path, &[("keep/me.txt", b"still here")]);
        let before = std::fs::read(&jar_path).unwrap();

        strip_zip_entry(&jar_path, "META-INF/mods.toml").unwrap();

        assert_eq!(
            std::fs::read(&jar_path).unwrap(),
            before,
            "nothing to strip: file must be untouched"
        );
    }
}
