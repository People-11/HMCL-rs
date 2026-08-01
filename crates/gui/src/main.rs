#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Slint 生成的类型在 `hmcl-ui` 里（拆开是为了改逻辑时不重编那 33 万行生成代码，
// 理由见 crates/ui/Cargo.toml）。
use hmcl_ui::*;

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use futures::{stream, StreamExt};
use hmcl_core::download::{
    modrinth, CacheRepository, DownloadProvider, InstallStage, ProgressEvent,
};
use hmcl_core::game_install::{self, LoaderKind, LoaderSelection};
use hmcl_core::install::{self, GameRepository};
use hmcl_core::launch;
use hmcl_core::modpack;
use hmcl_core::platform::Platform;
use hmcl_core::session::{self, LaunchEvent, LaunchRequest};
use hmcl_core::settings::accounts::{
    AccountsFile, AuthlibInjectorAccountEntry, AuthlibInjectorAccountTokensFile, KnownAccount,
    MicrosoftAccountEntry, MicrosoftAccountTokensFile, OfflineAccountEntry,
};
use hmcl_core::settings::authlib_injector_servers::{
    AuthlibInjectorServersFile, SCHEMA_ID as AUTHLIB_INJECTOR_SERVERS_SCHEMA_ID,
};
use hmcl_core::settings::game_directories::{
    GameDirectoriesFile, GameDirectory, LocalizedText, LOCAL_DEFAULT_ID,
};
use hmcl_core::settings::launcher_data_dir;
use hmcl_core::settings::launcher_settings::LauncherSettings;
use hmcl_core::version::Env;
use hmcl_core::versioning::GameVersionNumber;
use hmcl_core::world::World;
use serde::{Deserialize, Serialize};
use slint::Model;

static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

fn http_client() -> reqwest::Client {
    HTTP_CLIENT.clone()
}

fn accounts_file_path() -> PathBuf {
    launcher_data_dir().join("config").join("accounts.json")
}

const MICROSOFT_TOKENS_SCHEMA_ID: &str = "hmcl-rs-microsoft-account-tokens";

fn microsoft_tokens_file_path() -> PathBuf {
    launcher_data_dir()
        .join("private")
        .join("hmcl-rs-microsoft-accounts.json")
}

const AUTHLIB_INJECTOR_TOKENS_SCHEMA_ID: &str = "hmcl-rs-authlib-injector-account-tokens";

fn authlib_injector_servers_file_path() -> PathBuf {
    launcher_data_dir()
        .join("config")
        .join("authlib-injector-servers.json")
}

fn authlib_injector_tokens_file_path() -> PathBuf {
    launcher_data_dir()
        .join("private")
        .join("hmcl-rs-authlib-injector-accounts.json")
}

fn authlib_injector_artifact_path() -> PathBuf {
    launcher_data_dir()
        .join("libraries")
        .join("authlib-injector.jar")
}

fn account_skin_cache_path(profile_id: &str) -> PathBuf {
    launcher_data_dir()
        .join("cache")
        .join("account-skins")
        .join(format!("{profile_id}.png"))
}

async fn cache_microsoft_skin(
    client: &reqwest::Client,
    session: &hmcl_core::auth::microsoft::MicrosoftSession,
) -> Option<PathBuf> {
    let url = secure_minecraft_texture_url(session.skin_url.as_deref()?)?;
    let path = account_skin_cache_path(&session.profile_id);
    if path.is_file() {
        return Some(path);
    }
    tokio::fs::create_dir_all(path.parent()?).await.ok()?;
    let bytes = client
        .get(url.as_ref())
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .bytes()
        .await
        .ok()?;
    if bytes.len() > 2 * 1024 * 1024 {
        return None;
    }
    let temporary = path.with_extension("png.part");
    tokio::fs::write(&temporary, bytes).await.ok()?;
    tokio::fs::rename(&temporary, &path).await.ok()?;
    Some(path)
}

fn secure_minecraft_texture_url(url: &str) -> Option<std::borrow::Cow<'_, str>> {
    const HTTPS_PREFIX: &str = "https://textures.minecraft.net/texture/";
    const HTTP_PREFIX: &str = "http://textures.minecraft.net/texture/";
    if url.starts_with(HTTPS_PREFIX) {
        Some(url.into())
    } else {
        url.strip_prefix(HTTP_PREFIX)
            .map(|suffix| format!("{HTTPS_PREFIX}{suffix}").into())
    }
}

fn launcher_settings_path() -> PathBuf {
    launcher_data_dir()
        .join("config")
        .join("launcher-settings.json")
}

fn game_directories_file_path() -> PathBuf {
    launcher_data_dir()
        .join("config")
        .join("game-directories.json")
}

fn default_game_directory() -> GameDirectory {
    let mut directory = GameDirectory::new(Some(".minecraft".to_string()), ".minecraft");
    directory.id = LOCAL_DEFAULT_ID.to_string();
    directory
}

fn initialize_game_directories() {
    let path = game_directories_file_path();
    let mut loaded = hmcl_core::settings::load::<GameDirectoriesFile>(
        &path,
        hmcl_core::settings::game_directories::SCHEMA_ID,
    );
    const LEGACY_LOCAL_DEFAULT_ID: &str = "7105bc1f-490e-5e8c-878c-f5844c3d4bc3";
    let mut migrated_id = false;
    if loaded.can_save {
        for directory in &mut loaded.value.directories {
            if directory.id == LEGACY_LOCAL_DEFAULT_ID {
                directory.id = LOCAL_DEFAULT_ID.to_string();
                migrated_id = true;
            }
        }
    }
    if migrated_id {
        let mut settings = hmcl_core::settings::load::<LauncherSettings>(
            &launcher_settings_path(),
            hmcl_core::settings::launcher_settings::SCHEMA_ID,
        );
        if settings.value.selected_game_directory.as_deref() == Some(LEGACY_LOCAL_DEFAULT_ID) {
            settings.value.selected_game_directory = Some(LOCAL_DEFAULT_ID.to_string());
        }
        if let Some(instance) = settings
            .value
            .selected_instance
            .remove(LEGACY_LOCAL_DEFAULT_ID)
        {
            settings
                .value
                .selected_instance
                .insert(LOCAL_DEFAULT_ID.to_string(), instance);
        }
        if settings.can_save {
            let _ = hmcl_core::settings::save(
                &launcher_settings_path(),
                hmcl_core::settings::launcher_settings::SCHEMA_ID,
                &settings.value,
            );
        }
    }
    let added_default = loaded.value.directories.is_empty() && loaded.can_save;
    if added_default {
        loaded.value.directories.push(default_game_directory());
    }
    if (migrated_id || added_default) && loaded.can_save {
        let _ = hmcl_core::settings::save(
            &path,
            hmcl_core::settings::game_directories::SCHEMA_ID,
            &loaded.value,
        );
    }

    let directories = if loaded.value.directories.is_empty() {
        vec![default_game_directory()]
    } else {
        loaded.value.directories
    };
    let mut settings = hmcl_core::settings::load::<LauncherSettings>(
        &launcher_settings_path(),
        hmcl_core::settings::launcher_settings::SCHEMA_ID,
    );
    if !settings
        .value
        .selected_game_directory
        .as_ref()
        .is_some_and(|id| directories.iter().any(|directory| &directory.id == id))
    {
        settings.value.selected_game_directory =
            directories.first().map(|directory| directory.id.clone());
        if settings.can_save {
            let _ = hmcl_core::settings::save(
                &launcher_settings_path(),
                hmcl_core::settings::launcher_settings::SCHEMA_ID,
                &settings.value,
            );
        }
    }
}

fn game_directory_display_name(directory: &GameDirectory) -> String {
    match directory.name.as_ref() {
        Some(LocalizedText::Plain(name)) if !name.trim().is_empty() => name.clone(),
        Some(LocalizedText::ByLocale(names)) => names
            .get("zh_CN")
            .or_else(|| names.get("en"))
            .or_else(|| names.values().next())
            .cloned()
            .unwrap_or_else(|| directory.path.clone()),
        _ => directory.path.clone(),
    }
}

fn same_game_directory_path(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn suggested_game_directory_name(path: &Path) -> String {
    let leaf = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if leaf.eq_ignore_ascii_case(".minecraft") {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(".minecraft")
            .to_string()
    } else if leaf.is_empty() {
        path.display().to_string()
    } else {
        leaf.to_string()
    }
}

fn refresh_game_directories(ui: &AppWindow) {
    let loaded = hmcl_core::settings::load::<GameDirectoriesFile>(
        &game_directories_file_path(),
        hmcl_core::settings::game_directories::SCHEMA_ID,
    );
    let directories = if loaded.value.directories.is_empty() {
        vec![default_game_directory()]
    } else {
        loaded.value.directories
    };
    let selected_id = load_launcher_settings().selected_game_directory;
    let selected_index = selected_id
        .and_then(|id| directories.iter().position(|directory| directory.id == id))
        .unwrap_or(0);
    let rows = directories
        .into_iter()
        .map(|directory| {
            let name = game_directory_display_name(&directory);
            GameDirectoryRow {
                id: directory.id.into(),
                name: name.into(),
                path: directory.path.into(),
            }
        })
        .collect::<Vec<_>>();
    ui.set_game_directories(slint::ModelRc::new(slint::VecModel::from(rows)));
    ui.set_selected_game_directory_index(selected_index as i32);
}

fn set_selected_game_directory(directory_id: &str) -> Result<(), String> {
    let path = game_directories_file_path();
    let directories = hmcl_core::settings::load::<GameDirectoriesFile>(
        &path,
        hmcl_core::settings::game_directories::SCHEMA_ID,
    )
    .value;
    if !directories
        .directories
        .iter()
        .any(|directory| directory.id == directory_id)
    {
        return Err("找不到该游戏文件夹".to_string());
    }
    let settings_path = launcher_settings_path();
    let mut settings = hmcl_core::settings::load::<LauncherSettings>(
        &settings_path,
        hmcl_core::settings::launcher_settings::SCHEMA_ID,
    );
    if !settings.can_save {
        return Err("启动器设置版本不受支持，无法切换游戏文件夹".to_string());
    }
    settings.value.selected_game_directory = Some(directory_id.to_string());
    hmcl_core::settings::save(
        &settings_path,
        hmcl_core::settings::launcher_settings::SCHEMA_ID,
        &settings.value,
    )
    .map_err(|error| error.to_string())
}

fn reload_selected_game_directory(ui: &AppWindow) {
    refresh_game_directories(ui);
    let game_dir = resolve_game_dir();
    refresh_instances(ui, &game_dir, ui.get_filter_text().as_str());
    restore_selected_instance(ui);
}

fn load_launcher_settings() -> LauncherSettings {
    hmcl_core::settings::load::<LauncherSettings>(
        &launcher_settings_path(),
        hmcl_core::settings::launcher_settings::SCHEMA_ID,
    )
    .value
}

fn automatic_download_concurrency(processors: usize) -> usize {
    processors.saturating_mul(4).clamp(1, 64)
}

fn prefers_mirror_for_environment(
    utc_offset_seconds: i32,
    timezone: Option<&str>,
    locale: Option<&str>,
    geo_id: Option<i32>,
) -> bool {
    if timezone == Some("Asia/Shanghai") {
        return true;
    }
    if utc_offset_seconds != 8 * 60 * 60 {
        return false;
    }
    let locale = locale.unwrap_or_default().to_ascii_uppercase();
    locale.contains("_CN") || locale.contains("-CN") || geo_id == Some(45)
}

#[cfg(windows)]
fn windows_user_geo_id() -> Option<i32> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserGeoID(geo_class: i32) -> i32;
    }
    const GEOCLASS_NATION: i32 = 16;
    let id = unsafe { GetUserGeoID(GEOCLASS_NATION) };
    (id >= 0).then_some(id)
}

#[cfg(not(windows))]
fn windows_user_geo_id() -> Option<i32> {
    None
}

fn prefer_mirror_for_auto_downloads() -> bool {
    let timezone = std::env::var("TZ").ok();
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()));
    prefers_mirror_for_environment(
        chrono::Local::now().offset().local_minus_utc(),
        timezone.as_deref(),
        locale.as_deref(),
        windows_user_geo_id(),
    )
}

fn configured_download_provider(version_list: bool) -> DownloadProvider {
    use hmcl_core::settings::launcher_settings::DownloadSource;
    let settings = load_launcher_settings();
    let source = if version_list {
        settings.version_list_source
    } else {
        settings.file_download_source
    }
    .unwrap_or(DownloadSource::Default);
    let provider = match source {
        DownloadSource::Default => DownloadProvider::auto(prefer_mirror_for_auto_downloads()),
        DownloadSource::Official => DownloadProvider::mojang(),
        DownloadSource::Mirror => DownloadProvider::auto(true),
    };
    let concurrency = if settings.auto_download_threads.unwrap_or(true) {
        automatic_download_concurrency(
            std::thread::available_parallelism()
                .map(|processors| processors.get())
                .unwrap_or(1),
        )
    } else {
        settings.download_threads.unwrap_or(64) as usize
    };
    provider.with_concurrency(concurrency)
}

fn find_preferred_java(
    recommended_major: Option<u32>,
) -> Result<hmcl_core::java::JavaRuntime, hmcl_core::java::JavaDetectError> {
    if recommended_major.is_none() {
        if let Ok(system_java) = hmcl_core::java::find_a_java(None) {
            return Ok(system_java);
        }
    }

    let runtimes = find_java_runtimes();
    if let Some(recommended) = recommended_major {
        if let Some(exact) = runtimes
            .iter()
            .filter(|java| java.parsed_version() == Some(recommended))
            .max_by(|a, b| a.info.version.cmp(&b.info.version))
        {
            return Ok(exact.clone());
        }
        if let Some(newer) = runtimes
            .iter()
            .filter(|java| {
                java.parsed_version()
                    .is_some_and(|major| major > recommended)
            })
            .min_by_key(|java| java.parsed_version())
        {
            return Ok(newer.clone());
        }
    }

    runtimes
        .into_iter()
        .max_by_key(|java| java.parsed_version())
        .ok_or(hmcl_core::java::JavaDetectError::NotFound)
}

fn find_java_runtimes() -> Vec<hmcl_core::java::JavaRuntime> {
    let platform = Platform::CURRENT.to_string();
    let mut runtimes = Vec::new();
    for component in [
        "java-runtime-epsilon",
        "java-runtime-delta",
        "java-runtime-beta",
        "java-runtime-alpha",
        "jre-legacy",
    ] {
        let binary = launcher_data_dir()
            .join("java")
            .join(&platform)
            .join(format!("mojang-{component}"))
            .join("bin")
            .join("java.exe");
        if let Ok(java) = hmcl_core::java::java_runtime_from_binary(binary, true) {
            runtimes.push(java);
        }
    }
    if let Ok(java) = hmcl_core::java::find_a_java(None) {
        if !runtimes.iter().any(|runtime| runtime.binary == java.binary) {
            runtimes.push(java);
        }
    }
    runtimes.sort();
    runtimes
}

fn refresh_java_ui(ui: &AppWindow) {
    let rows: Vec<JavaRow> = find_java_runtimes()
        .into_iter()
        .map(|java| JavaRow {
            version: format!(
                "{} {}",
                if java.is_jdk { "JDK" } else { "JRE" },
                java.info.version
            )
            .into(),
            detail: format!(
                "架构: {}  ·  供应商: {}{}",
                java.architecture().checked_name().replace('_', "-"),
                java.info.vendor.as_deref().unwrap_or("未知"),
                if java.is_managed {
                    "  ·  HMCL 管理"
                } else {
                    ""
                }
            )
            .into(),
            path: java.binary.display().to_string().into(),
        })
        .collect();
    ui.set_java_runtimes(slint::ModelRc::new(slint::VecModel::from(rows)));
}

fn managed_java_binary(version: &str) -> Option<PathBuf> {
    let component = match version {
        "8" => "jre-legacy",
        "16" => "java-runtime-alpha",
        "17" => "java-runtime-beta",
        "21" => "java-runtime-delta",
        "25" => "java-runtime-epsilon",
        _ => return None,
    };
    let binary = launcher_data_dir()
        .join("java")
        .join(Platform::CURRENT.to_string())
        .join(format!("mojang-{component}"))
        .join("bin")
        .join("java.exe");
    binary.is_file().then_some(binary)
}

fn source_index(source: Option<hmcl_core::settings::launcher_settings::DownloadSource>) -> i32 {
    use hmcl_core::settings::launcher_settings::DownloadSource;
    match source.unwrap_or(DownloadSource::Default) {
        DownloadSource::Default => 0,
        DownloadSource::Official => 1,
        DownloadSource::Mirror => 2,
    }
}

fn source_from_index(index: i32) -> hmcl_core::settings::launcher_settings::DownloadSource {
    use hmcl_core::settings::launcher_settings::DownloadSource;
    match index {
        1 => DownloadSource::Official,
        2 => DownloadSource::Mirror,
        _ => DownloadSource::Default,
    }
}

fn populate_launcher_settings_ui(ui: &AppWindow, settings: &LauncherSettings) {
    ui.set_launcher_settings_syncing(true);
    ui.set_titlebar_transparent(settings.title_bar_transparent.unwrap_or(false));
    ui.set_animation_disabled(settings.animation_disabled.unwrap_or(false));
    ui.set_version_source_index(source_index(settings.version_list_source));
    ui.set_file_source_index(source_index(settings.file_download_source));
    ui.set_auto_download_threads(settings.auto_download_threads.unwrap_or(true));
    ui.set_download_threads(settings.download_threads.unwrap_or(64).to_string().into());
    ui.set_launcher_settings_syncing(false);
}

fn apply_ui_to_launcher_settings(ui: &AppWindow, settings: &mut LauncherSettings) {
    settings.title_bar_transparent = Some(ui.get_titlebar_transparent());
    settings.animation_disabled = Some(ui.get_animation_disabled());
    settings.version_list_source = Some(source_from_index(ui.get_version_source_index()));
    settings.file_download_source = Some(source_from_index(ui.get_file_source_index()));
    settings.auto_download_threads = Some(ui.get_auto_download_threads());
    settings.download_threads = ui
        .get_download_threads()
        .trim()
        .parse::<u32>()
        .ok()
        .map(|value| value.clamp(1, 256));
}

const GAME_SETTINGS_SCHEMA_ID: &str = "game-settings";
const DEFAULT_PRESET_ID: &str = "game-settings-preset:00000000-0000-0000-0000-000000000001";

fn game_settings_path() -> PathBuf {
    launcher_data_dir()
        .join("config")
        .join("game-settings.json")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GlobalGameSettingsPreset {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_isolation_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    java_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    custom_java_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    custom_java_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto_memory: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min_memory: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_memory: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    perm_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    launcher_visibility: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    height: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enable_debug_log_output: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    no_jvm_options: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    no_optimizing_jvm_options: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    jvm_options: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    game_arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quick_play: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quick_play_multiplayer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quick_play_singleplayer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quick_play_realms: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    process_priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    graphics_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment_variables: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command_wrapper: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pre_launch_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    post_exit_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    use_custom_natives: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    natives_directory: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GlobalGameSettingsFile {
    #[serde(default)]
    presets: Vec<GlobalGameSettingsPreset>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

fn selected_global_preset<'a>(
    settings: &'a LauncherSettings,
    file: &'a GlobalGameSettingsFile,
) -> Option<&'a GlobalGameSettingsPreset> {
    settings
        .default_game_settings_preset
        .as_deref()
        .and_then(|id| file.presets.iter().find(|preset| preset.id == id))
        .or_else(|| file.presets.first())
}

fn populate_global_settings_ui(
    ui: &AppWindow,
    launcher: &LauncherSettings,
    file: &GlobalGameSettingsFile,
) {
    let preset = selected_global_preset(launcher, file);
    ui.set_global_settings_syncing(true);
    ui.set_global_java_index(match preset.and_then(|p| p.java_type.as_deref()) {
        Some("CUSTOM") => 4,
        _ => match preset.and_then(|p| p.custom_java_version.as_deref()) {
            Some("8") => 1,
            Some("17") => 2,
            Some("21") => 3,
            _ => 0,
        },
    });
    ui.set_global_custom_java_path(
        preset
            .and_then(|p| p.custom_java_path.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_global_isolation_index(
        match preset.and_then(|p| p.default_isolation_type.as_deref()) {
            Some("NEVER") => 0,
            Some("ALWAYS") => 2,
            _ => 1,
        },
    );
    ui.set_global_auto_memory(preset.and_then(|p| p.auto_memory).unwrap_or(true));
    ui.set_global_max_memory(
        preset
            .and_then(|p| p.max_memory)
            .unwrap_or(2048)
            .to_string()
            .into(),
    );
    ui.set_global_min_memory(
        preset
            .and_then(|p| p.min_memory)
            .map(|value| value.to_string())
            .unwrap_or_default()
            .into(),
    );
    ui.set_global_perm_size(
        preset
            .and_then(|p| p.perm_size)
            .map(|value| value.to_string())
            .unwrap_or_default()
            .into(),
    );
    ui.set_global_launcher_visibility_index(
        match preset.and_then(|p| p.launcher_visibility.as_deref()) {
            Some("CLOSE") => 0,
            Some("HIDE") => 1,
            Some("HIDE_AND_REOPEN") => 3,
            _ => 2,
        },
    );
    ui.set_global_window_type_index(match preset.and_then(|p| p.window_type.as_deref()) {
        Some("FULLSCREEN") => 1,
        _ => 0,
    });
    ui.set_global_window_width(
        preset
            .and_then(|p| p.width)
            .unwrap_or(1280.0)
            .round()
            .to_string()
            .into(),
    );
    ui.set_global_window_height(
        preset
            .and_then(|p| p.height)
            .unwrap_or(720.0)
            .round()
            .to_string()
            .into(),
    );
    ui.set_global_debug_log(
        preset
            .and_then(|p| p.enable_debug_log_output)
            .unwrap_or(false),
    );
    ui.set_global_no_jvm_options(preset.and_then(|p| p.no_jvm_options).unwrap_or(false));
    ui.set_global_no_optimizing_jvm_options(
        preset
            .and_then(|p| p.no_optimizing_jvm_options)
            .unwrap_or(false),
    );
    ui.set_global_jvm_options(
        preset
            .and_then(|p| p.jvm_options.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_global_game_arguments(
        preset
            .and_then(|p| p.game_arguments.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_global_quick_play_index(match preset.and_then(|p| p.quick_play.as_deref()) {
        Some("MULTIPLAYER") => 1,
        Some("SINGLEPLAYER") => 2,
        Some("REALMS") => 3,
        _ => 0,
    });
    ui.set_global_quick_play_multiplayer(
        preset
            .and_then(|p| p.quick_play_multiplayer.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_global_quick_play_singleplayer(
        preset
            .and_then(|p| p.quick_play_singleplayer.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_global_quick_play_realms(
        preset
            .and_then(|p| p.quick_play_realms.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_global_process_priority_index(
        match preset.and_then(|p| p.process_priority.as_deref()) {
            Some("LOW") => 0,
            Some("BELOW_NORMAL") => 1,
            Some("ABOVE_NORMAL") => 3,
            Some("HIGH") => 4,
            _ => 2,
        },
    );
    ui.set_global_graphics_backend_index(
        match preset.and_then(|p| p.graphics_backend.as_deref()) {
            Some("OPENGL") => 1,
            Some("VULKAN") => 2,
            _ => 0,
        },
    );
    ui.set_global_environment_variables(
        preset
            .and_then(|p| p.environment_variables.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_global_command_wrapper(
        preset
            .and_then(|p| p.command_wrapper.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_global_pre_launch_command(
        preset
            .and_then(|p| p.pre_launch_command.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_global_post_exit_command(
        preset
            .and_then(|p| p.post_exit_command.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_global_use_custom_natives(preset.and_then(|p| p.use_custom_natives).unwrap_or(false));
    ui.set_global_natives_directory(
        preset
            .and_then(|p| p.natives_directory.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_global_settings_syncing(false);
}

fn apply_ui_to_global_preset(ui: &AppWindow, preset: &mut GlobalGameSettingsPreset) {
    preset.default_isolation_type = Some(
        match ui.get_global_isolation_index() {
            0 => "NEVER",
            2 => "ALWAYS",
            _ => "MODDED",
        }
        .to_string(),
    );
    let java_version = match ui.get_global_java_index() {
        1 => Some("8"),
        2 => Some("17"),
        3 => Some("21"),
        _ => None,
    };
    preset.java_type = Some(
        if ui.get_global_java_index() == 4 {
            "CUSTOM"
        } else if java_version.is_some() {
            "VERSION"
        } else {
            "AUTO"
        }
        .to_string(),
    );
    preset.custom_java_version = java_version.map(str::to_string);
    preset.custom_java_path = Some(ui.get_global_custom_java_path().to_string());
    preset.auto_memory = Some(ui.get_global_auto_memory());
    preset.min_memory = ui.get_global_min_memory().trim().parse().ok();
    preset.max_memory = ui
        .get_global_max_memory()
        .trim()
        .parse::<u32>()
        .ok()
        .map(|value| value.clamp(256, 131_072));
    preset.perm_size = ui.get_global_perm_size().trim().parse().ok();
    preset.launcher_visibility = Some(
        match ui.get_global_launcher_visibility_index() {
            0 => "CLOSE",
            2 => "KEEP",
            3 => "HIDE_AND_REOPEN",
            _ => "HIDE",
        }
        .to_string(),
    );
    preset.window_type = Some(
        if ui.get_global_window_type_index() == 1 {
            "FULLSCREEN"
        } else {
            "WINDOWED"
        }
        .to_string(),
    );
    preset.width = ui
        .get_global_window_width()
        .trim()
        .parse::<f64>()
        .ok()
        .map(|value| value.clamp(320.0, 16_384.0));
    preset.height = ui
        .get_global_window_height()
        .trim()
        .parse::<f64>()
        .ok()
        .map(|value| value.clamp(240.0, 16_384.0));
    preset.enable_debug_log_output = Some(ui.get_global_debug_log());
    preset.no_jvm_options = Some(ui.get_global_no_jvm_options());
    preset.no_optimizing_jvm_options = Some(ui.get_global_no_optimizing_jvm_options());
    preset.jvm_options = Some(ui.get_global_jvm_options().to_string());
    preset.game_arguments = Some(ui.get_global_game_arguments().to_string());
    preset.quick_play = Some(
        match ui.get_global_quick_play_index() {
            1 => "MULTIPLAYER",
            2 => "SINGLEPLAYER",
            3 => "REALMS",
            _ => "NONE",
        }
        .to_string(),
    );
    preset.quick_play_multiplayer = Some(ui.get_global_quick_play_multiplayer().to_string());
    preset.quick_play_singleplayer = Some(ui.get_global_quick_play_singleplayer().to_string());
    preset.quick_play_realms = Some(ui.get_global_quick_play_realms().to_string());
    preset.process_priority = Some(
        match ui.get_global_process_priority_index() {
            0 => "LOW",
            1 => "BELOW_NORMAL",
            3 => "ABOVE_NORMAL",
            4 => "HIGH",
            _ => "NORMAL",
        }
        .to_string(),
    );
    preset.graphics_backend = Some(
        match ui.get_global_graphics_backend_index() {
            1 => "OPENGL",
            2 => "VULKAN",
            _ => "DEFAULT",
        }
        .to_string(),
    );
    preset.environment_variables = Some(ui.get_global_environment_variables().to_string());
    preset.command_wrapper = Some(ui.get_global_command_wrapper().to_string());
    preset.pre_launch_command = Some(ui.get_global_pre_launch_command().to_string());
    preset.post_exit_command = Some(ui.get_global_post_exit_command().to_string());
    preset.use_custom_natives = Some(ui.get_global_use_custom_natives());
    preset.natives_directory = Some(ui.get_global_natives_directory().to_string());
}

/// 用户反馈"每次开都要重新选账户"——之前完全没读写 `selectedAccount`/
/// `selectedInstance` 这两个早就建好模的字段（`launcher_settings.rs`），现在接上：
/// 选中账户/实例时立刻写盘，启动时读回来。跟 Java 版共用同一份 `launcher-settings.
/// json`，切回真实 HMCL 也认得这两个字段。
fn set_selected_account(account_id: &str) {
    let path = launcher_settings_path();
    let mut loaded = hmcl_core::settings::load::<LauncherSettings>(
        &path,
        hmcl_core::settings::launcher_settings::SCHEMA_ID,
    );
    loaded.value.selected_account = Some(account_id.to_string());
    if loaded.can_save {
        let _ = hmcl_core::settings::save(
            &path,
            hmcl_core::settings::launcher_settings::SCHEMA_ID,
            &loaded.value,
        );
    }
}

fn game_directory_key() -> String {
    load_launcher_settings()
        .selected_game_directory
        .unwrap_or_else(|| "default".to_string())
}

fn set_selected_instance(instance_id: &str) {
    let path = launcher_settings_path();
    let mut loaded = hmcl_core::settings::load::<LauncherSettings>(
        &path,
        hmcl_core::settings::launcher_settings::SCHEMA_ID,
    );
    loaded
        .value
        .selected_instance
        .insert(game_directory_key(), instance_id.to_string());
    if loaded.can_save {
        let _ = hmcl_core::settings::save(
            &path,
            hmcl_core::settings::launcher_settings::SCHEMA_ID,
            &loaded.value,
        );
    }
}

fn restore_selected_account(ui: &AppWindow) {
    let accounts = hmcl_core::settings::load::<AccountsFile>(
        &accounts_file_path(),
        hmcl_core::settings::accounts::SCHEMA_ID,
    )
    .value
    .known_accounts();
    let index = load_launcher_settings().selected_account.and_then(|id| {
        accounts
            .iter()
            .position(|account| account.account_id() == id)
    });
    ui.set_selected_account_index(index.map(|i| i as i32).unwrap_or(-1));
}

fn restore_selected_instance(ui: &AppWindow) {
    let Some(selected_id) = load_launcher_settings()
        .selected_instance
        .remove(&game_directory_key())
    else {
        ui.set_selected_instance_index(-1);
        return;
    };
    let model = ui.get_instances();
    let index = (0..model.row_count()).find(|&i| {
        model
            .row_data(i)
            .is_some_and(|row| row.id.as_str() == selected_id)
    });
    ui.set_selected_instance_index(index.map(|i| i as i32).unwrap_or(-1));
}

fn resolve_game_dir() -> PathBuf {
    if let Some(selected_id) = load_launcher_settings().selected_game_directory {
        let dirs_path = launcher_data_dir()
            .join("config")
            .join("game-directories.json");
        let dirs = hmcl_core::settings::load::<GameDirectoriesFile>(
            &dirs_path,
            hmcl_core::settings::game_directories::SCHEMA_ID,
        )
        .value;
        if let Some(dir) = dirs.directories.iter().find(|d| d.id == selected_id) {
            return PathBuf::from(
                hmcl_core::settings::game_directories::normalize_portable_path(&dir.path),
            );
        }
    }

    PathBuf::from(".minecraft")
}

fn refresh_accounts(ui: &AppWindow) {
    let loaded = hmcl_core::settings::load::<AccountsFile>(
        &accounts_file_path(),
        hmcl_core::settings::accounts::SCHEMA_ID,
    );
    let accounts = loaded.value.known_accounts();
    let names: Vec<slint::SharedString> = accounts
        .iter()
        .map(|account| account.profile_name().into())
        .collect();
    ui.set_accounts(slint::ModelRc::new(slint::VecModel::from(names)));
    let kinds: Vec<slint::SharedString> = accounts
        .iter()
        .map(|account| account.kind().into())
        .collect();
    ui.set_account_kinds(slint::ModelRc::new(slint::VecModel::from(kinds)));
    let avatar_paths: Vec<Option<PathBuf>> = accounts
        .iter()
        .map(|account| match account {
            KnownAccount::Microsoft(account) => {
                let path = account_skin_cache_path(&account.profile_id);
                path.is_file().then_some(path)
            }
            KnownAccount::Offline(_) => None,
            KnownAccount::AuthlibInjector(_) => None,
        })
        .collect();
    ui.set_account_has_avatars(slint::ModelRc::new(slint::VecModel::from(
        avatar_paths.iter().map(Option::is_some).collect::<Vec<_>>(),
    )));
    ui.set_account_avatars(slint::ModelRc::new(slint::VecModel::from(
        avatar_paths
            .into_iter()
            .map(|path| {
                path.and_then(|path| slint::Image::load_from_path(&path).ok())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>(),
    )));
}

fn refresh_authlib_injector_servers(ui: &AppWindow) {
    let servers = hmcl_core::settings::load::<AuthlibInjectorServersFile>(
        &authlib_injector_servers_file_path(),
        AUTHLIB_INJECTOR_SERVERS_SCHEMA_ID,
    )
    .value
    .servers;
    ui.set_auth_server_names(slint::ModelRc::new(slint::VecModel::from(
        servers
            .iter()
            .map(|server| {
                slint::SharedString::from(if server.name.is_empty() {
                    server.url.as_str()
                } else {
                    server.name.as_str()
                })
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_auth_server_urls(slint::ModelRc::new(slint::VecModel::from(
        servers
            .iter()
            .map(|server| slint::SharedString::from(&server.url))
            .collect::<Vec<_>>(),
    )));
    ui.set_auth_server_non_email_login(slint::ModelRc::new(slint::VecModel::from(
        servers
            .iter()
            .map(|server| server.non_email_login)
            .collect::<Vec<_>>(),
    )));
}

fn instance_loader_kind(
    instance: &hmcl_core::version::Version,
    versions: &std::collections::HashMap<String, hmcl_core::version::Version>,
) -> Option<LoaderKind> {
    instance
        .patches
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find_map(|patch| LoaderKind::from_slug(&patch.id))
        .or_else(|| {
            instance
                .resolve(versions)
                .ok()
                .and_then(|resolved| modrinth::detect_loader(&resolved))
                .and_then(LoaderKind::from_slug)
        })
}

thread_local! {
    static INSTANCE_ROWS_CACHE: RefCell<Option<(PathBuf, Vec<InstanceRow>)>> =
        const { RefCell::new(None) };
}

fn set_filtered_instances(ui: &AppWindow, rows: &[InstanceRow], filter: &str) {
    let filter = filter.to_lowercase();
    let visible = rows
        .iter()
        .filter(|row| filter.is_empty() || row.id.to_lowercase().contains(&filter))
        .cloned()
        .collect::<Vec<_>>();
    ui.set_instances(slint::ModelRc::new(slint::VecModel::from(visible)));
}

fn refresh_instances(ui: &AppWindow, game_dir: &Path, filter: &str) {
    let repo = GameRepository::new(game_dir);
    let all = repo.load_all_versions();
    let mut ids: Vec<&String> = all.keys().filter(|id| !all[*id].is_hidden()).collect();
    ids.sort();

    let rows = ids
        .into_iter()
        .map(|id| InstanceRow {
            id: id.clone().into(),
            subtitle: "".into(),
            loader_kind: instance_loader_kind(&all[id], &all)
                .map(loader_kind_index)
                .unwrap_or(0),
        })
        .collect::<Vec<_>>();
    set_filtered_instances(ui, &rows, filter);
    INSTANCE_ROWS_CACHE.with(|cache| {
        *cache.borrow_mut() = Some((game_dir.to_path_buf(), rows));
    });
}

fn filter_instances(ui: &AppWindow, game_dir: &Path, filter: &str) {
    let used_cache = INSTANCE_ROWS_CACHE.with(|cache| {
        let cache = cache.borrow();
        let Some((cached_dir, rows)) = cache.as_ref() else {
            return false;
        };
        if cached_dir != game_dir {
            return false;
        }
        set_filtered_instances(ui, rows, filter);
        true
    });
    if !used_cache {
        refresh_instances(ui, game_dir, filter);
    }
}

fn is_april_fools_version(id: &str) -> bool {
    matches!(
        id,
        "15w14a"
            | "1.RV-Pre1"
            | "3D Shareware v1.34"
            | "20w14infinite"
            | "22w13oneblockatatime"
            | "23w13a_or_b"
            | "24w14potato"
            | "25w14craftmine"
            | "26w14a"
    )
}

fn version_type_matches(entry: &install::VersionManifestEntry, type_index: i32) -> bool {
    use hmcl_core::version::ReleaseType;
    match type_index {
        0 => entry.release_type == Some(ReleaseType::Release),
        1 => {
            entry.release_type == Some(ReleaseType::Snapshot) && !is_april_fools_version(&entry.id)
        }
        2 => is_april_fools_version(&entry.id),
        3 => matches!(
            entry.release_type,
            Some(ReleaseType::OldBeta) | Some(ReleaseType::OldAlpha)
        ),
        _ => true,
    }
}

fn version_type_label(entry: &install::VersionManifestEntry) -> &'static str {
    use hmcl_core::version::ReleaseType;
    if is_april_fools_version(&entry.id) {
        return "愚人节版";
    }
    match entry.release_type {
        Some(ReleaseType::Release) => "正式版",
        Some(ReleaseType::Snapshot) => "快照",
        Some(ReleaseType::OldBeta) => "远古 Beta",
        Some(ReleaseType::OldAlpha) => "远古 Alpha",
        _ => "其它",
    }
}

fn apply_remote_filter(ui: &AppWindow, manifest: &[install::VersionManifestEntry]) {
    let name_filter = ui.get_remote_filter_name().to_lowercase();
    let type_index = ui.get_remote_type_index();

    let rows: Vec<VersionRow> = manifest
        .iter()
        .filter(|e| version_type_matches(e, type_index))
        .filter(|e| name_filter.is_empty() || e.id.to_lowercase().contains(&name_filter))
        .map(|e| {
            let date = match e.release_date_parts() {
                Some((y, mo, d, h, mi, s)) => format!("{y} 年 {mo} 月 {d} 日 {h}:{mi}:{s}"),
                None => String::new(),
            };
            VersionRow {
                id: e.id.clone().into(),
                type_label: version_type_label(e).into(),
                date: date.into(),
            }
        })
        .collect();
    ui.set_remote_versions(slint::ModelRc::new(slint::VecModel::from(rows)));
}

fn apply_loader_filter(ui: &AppWindow, versions: &[String]) {
    let filter = ui.get_loader_filter_text().to_lowercase();
    let rows = versions
        .iter()
        .filter(|version| filter.is_empty() || version.to_lowercase().contains(&filter))
        .cloned()
        .map(slint::SharedString::from)
        .collect::<Vec<_>>();
    ui.set_loader_version_options(slint::ModelRc::new(slint::VecModel::from(rows)));
}

fn selected_instance_id(ui: &AppWindow) -> Option<String> {
    let index = ui.get_selected_instance_index();
    if index < 0 {
        return None;
    }
    ui.get_instances()
        .row_data(index as usize)
        .map(|row| row.id.to_string())
}

fn resolve_instance_context(
    game_dir: &Path,
    instance_id: &str,
) -> Option<(String, Option<&'static str>)> {
    let repo = GameRepository::new(game_dir);
    let all = repo.load_all_versions();
    let instance = all.get(instance_id)?;
    let resolved = instance.resolve(&all).ok()?;
    let loader = instance_loader_kind(instance, &all).map(LoaderKind::slug);
    Some((game_install::game_version_of(instance, &resolved), loader))
}

const MOD_SEARCH_PAGE_SIZE: u64 = 20;
const ONLINE_SEARCH_MAX_ROWS: usize = 400;

#[derive(Default)]
struct ModCategoryState {
    kind: i32,
    slugs: Vec<String>,
}

#[derive(Default)]
struct ModDetailState {
    versions: Vec<modrinth::ProjectVersion>,
    expanded: BTreeSet<String>,
}

struct PendingModRow {
    project_id: String,
    title: String,
    description: String,
    downloads: String,
    categories: String,
    icon_path: Option<PathBuf>,
}

/// `mod-search-kind`(slint 属性) -> (Modrinth project_type, 装完落在运行目录下
/// 的哪个子目录, 要不要按探测出的加载器过滤)。资源包/光影不按加载器过滤——
/// 它们不像模组那样绑定"fabric/forge"这种加载器分类。
fn mod_search_kind_info(kind: i32) -> (modrinth::ProjectType, &'static str, bool) {
    match kind {
        1 => (modrinth::ProjectType::ResourcePack, "resourcepacks", false),
        2 => (modrinth::ProjectType::Shader, "shaderpacks", false),
        3 => (modrinth::ProjectType::Modpack, "", false),
        _ => (modrinth::ProjectType::Mod, "mods", true),
    }
}

fn localize_modrinth_tag(tag: &str) -> String {
    match tag {
        "adventure" => "冒险",
        "atmosphere" => "氛围",
        "audio" => "声音",
        "blocks" => "方块",
        "cartoon" => "卡通",
        "challenging" => "高难度",
        "combat" => "战斗",
        "decoration" => "装饰",
        "economy" => "经济",
        "environment" => "环境",
        "equipment" => "装备",
        "fantasy" => "幻想",
        "food" => "食物",
        "game-mechanics" => "游戏机制",
        "library" => "支持库",
        "lightweight" => "轻量",
        "magic" => "魔法",
        "management" => "管理",
        "mobs" => "生物",
        "optimization" => "优化",
        "quests" => "任务",
        "realistic" => "写实",
        "simplistic" => "简单",
        "social" => "社交",
        "storage" => "存储",
        "technology" => "科技",
        "transportation" => "运输",
        "utility" => "实用",
        "vanilla-like" => "类原生",
        "worldgen" => "世界生成",
        "fabric" => "Fabric",
        "forge" => "Forge",
        "neoforge" => "NeoForge",
        "quilt" => "Quilt",
        "iris" => "Iris",
        other => other,
    }
    .to_string()
}

fn modrinth_sort_index(index: i32) -> &'static str {
    match index {
        1 => "newest",
        2 => "updated",
        3 => "downloads",
        _ => "relevance",
    }
}

async fn load_project_icon(
    client: &reqwest::Client,
    cache_dir: &Path,
    project_id: &str,
    icon_url: Option<&str>,
) -> Option<PathBuf> {
    let icon_url = icon_url?;
    let extension = icon_url
        .split('?')
        .next()
        .and_then(|url| Path::new(url).extension())
        .and_then(|extension| extension.to_str())
        .filter(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg"
            )
        })
        .unwrap_or("png");
    let path = cache_dir.join(format!("{project_id}.{extension}"));
    if !path.is_file() {
        if tokio::fs::create_dir_all(cache_dir).await.is_err() {
            return None;
        }
        let Ok(response) = client.get(icon_url).send().await else {
            return None;
        };
        let Ok(response) = response.error_for_status() else {
            return None;
        };
        let Ok(bytes) = response.bytes().await else {
            return None;
        };
        if tokio::fs::write(&path, bytes).await.is_err() {
            return None;
        }
    }
    Some(path)
}

fn format_modrinth_date(date: &str) -> String {
    date.get(..19)
        .unwrap_or(date)
        .replace('T', " ")
        .replace('-', " / ")
}

fn loader_labels(loaders: &[String]) -> String {
    loaders
        .iter()
        .filter_map(|loader| match loader.as_str() {
            "fabric" => Some("Fabric"),
            "forge" => Some("Forge"),
            "neoforge" => Some("NeoForge"),
            "quilt" => Some("Quilt"),
            "legacy-fabric" => Some("Legacy Fabric"),
            "liteloader" => Some("LiteLoader"),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("  ")
}

fn channel_label(channel: &str) -> &'static str {
    match channel {
        "beta" => "测试版",
        "alpha" => "开发版本",
        _ => "正式版",
    }
}

fn sorted_game_versions(versions: &[modrinth::ProjectVersion]) -> Vec<String> {
    let mut game_versions = versions
        .iter()
        .flat_map(|version| version.game_versions.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    game_versions.sort_by(|a, b| {
        GameVersionNumber::compare(b, a).unwrap_or_else(|| {
            if a == b {
                Ordering::Equal
            } else {
                b.cmp(a)
            }
        })
    });
    game_versions
}

fn mod_detail_rows(state: &ModDetailState) -> Vec<ModVersionRow> {
    let mut rows = Vec::new();
    for game_version in sorted_game_versions(&state.versions) {
        let expanded = state.expanded.contains(&game_version);
        rows.push(ModVersionRow {
            header: true,
            game_version: game_version.clone().into(),
            version_id: "".into(),
            file_name: "".into(),
            title: "".into(),
            date: "".into(),
            channel: "".into(),
            loaders: "".into(),
            expanded,
        });
        for version in state
            .versions
            .iter()
            .filter(|version| version.game_versions.contains(&game_version))
        {
            let file_name = version
                .files
                .iter()
                .find(|file| file.primary)
                .or_else(|| version.files.first())
                .map(|file| file.filename.clone())
                .unwrap_or_default();
            rows.push(ModVersionRow {
                header: false,
                game_version: game_version.clone().into(),
                version_id: version.id.clone().into(),
                file_name: file_name.into(),
                title: if version.name.is_empty() {
                    version.version_number.clone()
                } else {
                    version.name.clone()
                }
                .into(),
                date: format_modrinth_date(&version.date_published).into(),
                channel: channel_label(&version.version_type).into(),
                loaders: loader_labels(&version.loaders).into(),
                expanded,
            });
        }
    }
    rows
}

fn toggle_mod_detail_group(expanded: &mut BTreeSet<String>, game_version: String) {
    if !expanded.remove(&game_version) {
        expanded.clear();
        expanded.insert(game_version);
    }
}

fn set_mod_detail_rows(ui: &AppWindow, rows: Vec<ModVersionRow>) {
    let model = ui.get_mod_detail_versions();
    if let Some(existing) = model
        .as_any()
        .downcast_ref::<slint::VecModel<ModVersionRow>>()
        .filter(|existing| existing.row_count() == rows.len())
    {
        for (index, row) in rows.into_iter().enumerate() {
            existing.set_row_data(index, row);
        }
    } else {
        ui.set_mod_detail_versions(slint::ModelRc::new(slint::VecModel::from(rows)));
    }
}

async fn run_mod_search(
    ui_weak: slint::Weak<AppWindow>,
    game_dir: PathBuf,
    instance_id: String,
    query: String,
    kind: i32,
    game_version_filter: Option<String>,
    category_filter: Option<String>,
    sort_index: i32,
    page: u64,
    category_state: Arc<Mutex<ModCategoryState>>,
    search_generation: Arc<std::sync::atomic::AtomicU64>,
    request_generation: u64,
) {
    let (project_type, _dest_subdir, filter_by_loader) = mod_search_kind_info(kind);
    let context = resolve_instance_context(&game_dir, &instance_id);
    let context_loader = context.as_ref().and_then(|(_, loader)| *loader);
    let game_version = game_version_filter
        .as_deref()
        .or_else(|| context.as_ref().map(|(version, _)| version.as_str()));
    let loader = if filter_by_loader {
        context_loader
    } else {
        None
    };

    let client = http_client();
    let provider = configured_download_provider(false);
    let concurrency = provider.concurrency();
    let (result, categories) = tokio::join!(
        modrinth::search_projects(
            &client,
            &provider,
            project_type,
            &query,
            game_version,
            category_filter.as_deref(),
            loader,
            modrinth_sort_index(sort_index),
            page * MOD_SEARCH_PAGE_SIZE,
            MOD_SEARCH_PAGE_SIZE,
        ),
        async {
            if page == 0 {
                modrinth::fetch_categories(&client, &provider, project_type).await
            } else {
                Ok(Vec::new())
            }
        },
    );
    let icon_dir = game_dir.join(".hmcl-rs-cache").join("project-icons");
    let result = match result {
        Ok(resp) => {
            let total_hits = resp.total_hits;
            let offset = resp.offset;
            let rows = stream::iter(resp.hits.into_iter().map(|hit| {
                let client = client.clone();
                let icon_dir = icon_dir.clone();
                async move {
                    let icon = load_project_icon(
                        &client,
                        &icon_dir,
                        &hit.project_id,
                        hit.icon_url.as_deref(),
                    )
                    .await;
                    let tags = if hit.display_categories.is_empty() {
                        hit.categories
                    } else {
                        hit.display_categories
                    };
                    PendingModRow {
                        project_id: hit.project_id,
                        title: hit.title,
                        description: hit.description,
                        downloads: hit.downloads.to_string(),
                        categories: tags
                            .iter()
                            .map(|tag| localize_modrinth_tag(tag))
                            .collect::<Vec<_>>()
                            .join("  "),
                        icon_path: icon,
                    }
                }
            }))
            .buffered(concurrency)
            .collect::<Vec<_>>()
            .await;
            Ok((rows, offset, total_hits))
        }
        Err(error) => Err(error),
    };

    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        if search_generation.load(std::sync::atomic::Ordering::Relaxed) != request_generation {
            return;
        }
        ui.set_mod_search_loading(false);
        if page == 0 {
            if let Ok(slugs) = categories {
                let selected_slug = category_filter.as_deref();
                let selected_index = selected_slug
                    .and_then(|selected| slugs.iter().position(|slug| slug == selected))
                    .map(|index| index + 1)
                    .unwrap_or(0);
                let mut labels = vec![slint::SharedString::from("全部")];
                labels.extend(
                    slugs
                        .iter()
                        .map(|slug| slint::SharedString::from(localize_modrinth_tag(slug))),
                );
                *category_state.lock().unwrap() = ModCategoryState { kind, slugs };
                ui.set_mod_category_options(slint::ModelRc::new(slint::VecModel::from(labels)));
                ui.set_mod_category_index(selected_index as i32);
            }
        }
        match result {
            Ok((pending_rows, offset, total_hits)) => {
                let count = pending_rows.len();
                let has_more = offset + (count as u64) < total_hits;
                let rows = pending_rows
                    .into_iter()
                    .map(|row| ModRow {
                        project_id: row.project_id.into(),
                        title: row.title.into(),
                        description: row.description.into(),
                        downloads: row.downloads.into(),
                        categories: row.categories.into(),
                        icon: row
                            .icon_path
                            .as_deref()
                            .and_then(|path| slint::Image::load_from_path(path).ok())
                            .unwrap_or_default(),
                    })
                    .collect::<Vec<_>>();
                if page == 0 {
                    ui.set_mod_search_results(slint::ModelRc::new(slint::VecModel::from(rows)));
                } else {
                    let model = ui.get_mod_search_results();
                    if let Some(existing) = model.as_any().downcast_ref::<slint::VecModel<ModRow>>()
                    {
                        while existing.row_count() + rows.len() > ONLINE_SEARCH_MAX_ROWS {
                            existing.remove(0);
                        }
                        existing.extend(rows);
                    }
                }
                ui.set_mod_search_has_more(has_more);
                ui.set_mod_search_page(page as i32);
                ui.set_status_text(format!("共 {total_hits} 个结果").into());
            }
            Err(e) => {
                if page == 0 {
                    ui.set_mod_search_results(slint::ModelRc::new(slint::VecModel::from(Vec::<
                        ModRow,
                    >::new(
                    ))));
                }
                ui.set_status_text(format!("搜索失败: {e}").into());
            }
        }
    });
}

async fn run_mod_detail(
    ui_weak: slint::Weak<AppWindow>,
    project_id: String,
    preferred_game_version: Option<String>,
    detail_state: Arc<Mutex<ModDetailState>>,
) {
    let client = http_client();
    let provider = configured_download_provider(false);
    let result =
        modrinth::fetch_project_versions(&client, &provider, &project_id, None, None).await;
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.set_mod_detail_loading(false);
        match result {
            Ok(versions) => {
                let game_versions = sorted_game_versions(&versions);
                let first = preferred_game_version
                    .filter(|preferred| game_versions.contains(preferred))
                    .or_else(|| game_versions.first().cloned());
                let mut state = ModDetailState {
                    versions,
                    expanded: BTreeSet::new(),
                };
                if let Some(first) = first {
                    state.expanded.insert(first);
                }
                let rows = mod_detail_rows(&state);
                *detail_state.lock().unwrap() = state;
                set_mod_detail_rows(&ui, rows);
            }
            Err(error) => ui.set_status_text(format!("获取项目版本失败: {error}").into()),
        }
    });
}

fn start_mod_search(
    ui: &AppWindow,
    ui_weak: &slint::Weak<AppWindow>,
    handle: &tokio::runtime::Handle,
    game_dir: &Path,
    category_state: &Arc<Mutex<ModCategoryState>>,
    search_generation: &Arc<std::sync::atomic::AtomicU64>,
    page: u64,
) {
    let kind = ui.get_mod_search_kind();
    let request_generation = if page == 0 {
        ui.set_mod_search_results(slint::ModelRc::new(slint::VecModel::from(
            Vec::<ModRow>::new(),
        )));
        ui.set_mod_search_has_more(false);
        ui.set_mod_search_page(0);
        search_generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    } else {
        search_generation.load(std::sync::atomic::Ordering::Relaxed)
    };
    let instance_id = if kind == 3 {
        String::new()
    } else if ui.get_download_return_instance() {
        ui.get_settings_instance_id().to_string()
    } else {
        let Some(instance_id) = selected_instance_id(ui) else {
            ui.set_mod_search_loading(false);
            ui.set_status_text("请先选择一个游戏实例".into());
            return;
        };
        instance_id
    };
    let game_version_index = ui.get_mod_game_version_index();
    let game_version = (game_version_index > 0)
        .then(|| {
            ui.get_mod_game_version_options()
                .row_data(game_version_index as usize)
                .map(|version| version.to_string())
        })
        .flatten();
    let category_index = ui.get_mod_category_index();
    let category = {
        let state = category_state.lock().unwrap();
        (state.kind == kind && category_index > 0)
            .then(|| state.slugs.get(category_index as usize - 1).cloned())
            .flatten()
    };
    ui.set_mod_search_loading(true);
    let kind_label = match kind {
        1 => "资源包",
        2 => "光影",
        3 => "整合包",
        _ => "模组",
    };
    ui.set_status_text(format!("正在搜索{kind_label}…").into());
    handle.spawn(run_mod_search(
        ui_weak.clone(),
        game_dir.to_path_buf(),
        instance_id,
        ui.get_mod_search_query().to_string(),
        kind,
        game_version,
        category,
        ui.get_mod_sort_index(),
        page,
        category_state.clone(),
        search_generation.clone(),
        request_generation,
    ));
}

/// 三种"安装整合包"来源（本地文件/直链/Modrinth 搜索, 见
/// [`modpack::import_mrpack`]/[`modpack::import_from_url`]/
/// [`modpack::import_from_modrinth`]）跑完之后要做的事完全一样, 抽出来一份。
fn finish_modpack_import(
    ui: &AppWindow,
    game_dir: &Path,
    instance_id: &str,
    result: Result<install::InstallReport, modpack::ModpackError>,
) {
    ui.set_modpack_import_loading(false);
    match result {
        Ok(_) => {
            ui.set_status_text(format!("整合包已导入为实例 {instance_id}").into());
            ui.set_modpack_import_path("".into());
            ui.set_modpack_download_url("".into());
            ui.set_modpack_import_instance_name("".into());
            set_selected_instance(instance_id);
            refresh_instances(ui, game_dir, "");
            restore_selected_instance(ui);
            ui.set_install_succeeded(true);
        }
        Err(e) => {
            ui.set_show_install_progress(false);
            ui.set_status_text(format!("导入失败: {e}").into());
        }
    }
}

fn rename_instance(game_dir: &Path, old_id: &str, new_id: &str) -> Result<(), String> {
    let new_id = new_id.trim();
    if new_id.is_empty() || new_id == old_id {
        return Ok(());
    }
    let repo = GameRepository::new(game_dir);
    let all = repo.load_all_versions();
    if all.contains_key(new_id) {
        return Err(format!("{new_id} 已经存在"));
    }
    if all
        .iter()
        .any(|(id, v)| id != old_id && v.inherits_from.as_deref() == Some(old_id))
    {
        return Err("有其它实例继承自它, 暂不支持改名".to_string());
    }
    let mut renamed = all
        .get(old_id)
        .ok_or_else(|| format!("找不到实例 {old_id}"))?
        .clone();
    renamed.id = new_id.to_string();

    let old_root = repo.version_root(old_id);
    let new_root = repo.version_root(new_id);
    std::fs::rename(&old_root, &new_root).map_err(|e| e.to_string())?;
    let old_jar = new_root.join(format!("{old_id}.jar"));
    if old_jar.is_file() {
        let _ = std::fs::rename(&old_jar, new_root.join(format!("{new_id}.jar")));
    }
    let _ = std::fs::remove_file(new_root.join(format!("{old_id}.json")));
    repo.save_version_json(&renamed).map_err(|e| e.to_string())
}

fn loader_name_base(mut instance_id: &str) -> &str {
    loop {
        let stripped = LoaderKind::ALL.into_iter().find_map(|kind| {
            instance_id
                .strip_suffix(&format!("-{}", kind.slug()))
                .or_else(|| {
                    let marker = format!("-{}-", kind.slug());
                    let (base, number) = instance_id.rsplit_once(&marker)?;
                    number
                        .parse::<usize>()
                        .ok()
                        .filter(|number| *number >= 2)
                        .map(|_| base)
                })
        });
        match stripped {
            Some(base) => instance_id = base,
            None => return instance_id,
        }
    }
}

fn loader_instance_name(instance_id: &str, loader: Option<LoaderKind>) -> String {
    let base = loader_name_base(instance_id);
    loader
        .map(|kind| format!("{base}-{}", kind.slug()))
        .unwrap_or_else(|| base.to_string())
}

fn sync_instance_loader_name(
    game_dir: &Path,
    instance_id: &str,
    loader: Option<LoaderKind>,
) -> Result<String, String> {
    let desired = loader_instance_name(instance_id, loader);
    if desired == instance_id {
        return Ok(desired);
    }
    let repo = GameRepository::new(game_dir);
    let versions = repo.load_all_versions();
    if loader.is_none() {
        if let (Some(instance), Some(parent)) = (versions.get(instance_id), versions.get(&desired))
        {
            if instance.inherits_from.as_deref() == Some(desired.as_str()) && parent.is_hidden() {
                let old_root = repo.version_root(instance_id);
                let parent_root = repo.version_root(&desired);
                copy_dir_recursive(&old_root, &parent_root).map_err(|e| e.to_string())?;
                let _ = std::fs::remove_file(parent_root.join(format!("{instance_id}.json")));
                let _ = std::fs::remove_file(parent_root.join(format!("{instance_id}.jar")));

                let mut visible_parent = parent.clone();
                visible_parent.hidden = Some(false);
                repo.save_version_json(&visible_parent)
                    .map_err(|e| e.to_string())?;
                std::fs::remove_dir_all(old_root).map_err(|e| e.to_string())?;
                return Ok(desired);
            }
        }
    }
    let available = if !versions.contains_key(&desired) {
        desired
    } else {
        (2..)
            .map(|number| format!("{desired}-{number}"))
            .find(|candidate| !versions.contains_key(candidate))
            .unwrap()
    };
    rename_instance(game_dir, instance_id, &available)?;
    Ok(available)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}

/// 复制出的新实例自己取名`<id>-copy`/`<id>-copy2`/...——不弹框问名字，跟很多桌面
/// 应用"先复制一份, 用户自己再改名"的习惯一致（见
/// [`rename_instance`]）。复制完之后要把 json 里的 `id` 字段也改成新名字，不然
/// 磁盘上的目录名和文件内容的 id 对不上。
fn duplicate_instance(game_dir: &Path, id: &str) -> Result<String, String> {
    let repo = GameRepository::new(game_dir);
    let all = repo.load_all_versions();
    let source = all
        .get(id)
        .ok_or_else(|| format!("找不到实例 {id}"))?
        .clone();

    let mut new_id = format!("{id}-copy");
    let mut n = 2;
    while all.contains_key(&new_id) {
        new_id = format!("{id}-copy{n}");
        n += 1;
    }

    copy_dir_recursive(&repo.version_root(id), &repo.version_root(&new_id))
        .map_err(|e| e.to_string())?;
    let mut copy = source;
    copy.id = new_id.clone();
    repo.save_version_json(&copy).map_err(|e| e.to_string())?;
    Ok(new_id)
}

fn delete_instance(game_dir: &Path, id: &str) -> Result<(), String> {
    let repo = GameRepository::new(game_dir);
    std::fs::remove_dir_all(repo.version_root(id)).map_err(|e| e.to_string())
}

/// 在资源管理器里打开一个目录（不存在就先建出来）。全 app 有 6 处要干这件事。
/// 弹一个"选择文件"对话框，选中之后回到 UI 线程执行 `then`；取消就把 `cancelled`
/// 写进状态栏。全 app 有 4 处要走这一套 dialog -> spawn -> upgrade_in_event_loop
/// 的流程，每处都是同样十几行样板。
fn pick_files_then(
    ui_weak: slint::Weak<AppWindow>,
    handle: &tokio::runtime::Handle,
    dialog: rfd::AsyncFileDialog,
    cancelled: &'static str,
    then: impl FnOnce(&AppWindow, Vec<PathBuf>) + Send + 'static,
) {
    handle.spawn(async move {
        let Some(files) = dialog.pick_files().await else {
            set_status(&ui_weak, cancelled.to_string());
            return;
        };
        let paths: Vec<PathBuf> = files.iter().map(|file| file.path().to_path_buf()).collect();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| then(&ui, paths));
    });
}

fn with_extension(path: &Path, extension: &str) -> PathBuf {
    if path
        .extension()
        .is_some_and(|existing| existing.eq_ignore_ascii_case(extension))
    {
        path.to_path_buf()
    } else {
        path.with_extension(extension)
    }
}

fn reveal_directory(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer")
        .arg(dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn open_url(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("只允许打开 HTTP(S) 链接".to_string());
    }
    #[cfg(windows)]
    let mut command = {
        let mut command = std::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn minecraft_wiki_url(version: &str) -> String {
    let bytes = version.as_bytes();
    let snapshot = bytes.len() >= 6
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b'w'
        && bytes[3].is_ascii_digit()
        && bytes[4].is_ascii_digit();
    let page = if snapshot || is_april_fools_version(version) {
        version.to_string()
    } else {
        format!("Java版{version}")
    };
    let mut url = reqwest::Url::parse("https://zh.minecraft.wiki/w/").unwrap();
    url.path_segments_mut().unwrap().pop_if_empty().push(&page);
    url.query_pairs_mut().append_pair("variant", "zh-cn");
    url.into()
}

fn open_run_folder(game_dir: &Path, id: &str) -> Result<(), String> {
    let repo = GameRepository::new(game_dir);
    reveal_directory(&repo.run_directory(id))
}

fn open_java_folder(binary: &Path) -> Result<(), String> {
    let dir = binary
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| format!("无法确定 {} 的 Java 主目录", binary.display()))?;
    if !dir.is_dir() {
        return Err(format!("Java 主目录不存在: {}", dir.display()));
    }
    reveal_directory(dir)
}

fn instance_content_directory(
    game_dir: &Path,
    instance_id: &str,
    kind: i32,
) -> Result<PathBuf, String> {
    let run_dir = GameRepository::new(game_dir).run_directory(instance_id);
    let child = match kind {
        2 => "mods",
        3 => "resourcepacks",
        4 => "shaderpacks",
        5 => "saves",
        _ => return Err("该页面没有内容目录".to_string()),
    };
    Ok(run_dir.join(child))
}

fn local_content_rows(
    game_dir: &Path,
    instance_id: &str,
    kind: i32,
) -> Result<Vec<InstanceContentRow>, String> {
    if kind == 1 {
        let repo = GameRepository::new(game_dir);
        let versions = repo.load_all_versions();
        let version = versions
            .get(instance_id)
            .ok_or_else(|| format!("找不到实例 {instance_id}"))?;
        let resolved = version.resolve(&versions).map_err(|e| e.to_string())?;
        let game_version = game_install::game_version_of(version, &resolved);
        let installed = version
            .patches
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find_map(|patch| LoaderKind::from_slug(&patch.id).map(|kind| (kind, patch)));
        let mut rows = vec![InstanceContentRow {
            file_name: "".into(),
            name: "Minecraft".into(),
            detail: game_version.into(),
            enabled: true,
            directory: false,
            online_icon: false,
            icon: Default::default(),
        }];
        for kind in LoaderKind::ALL {
            let is_installed = installed.is_some_and(|(installed, _)| installed == kind);
            let detail = match installed {
                Some((installed, patch)) if installed == kind => {
                    patch.version.as_deref().unwrap_or("已安装").to_string()
                }
                Some((installed, _)) => {
                    format!("与 {} 不兼容", installed.display_name())
                }
                None => "未安装".to_string(),
            };
            rows.push(InstanceContentRow {
                file_name: kind.slug().into(),
                name: kind.display_name().into(),
                detail: detail.into(),
                enabled: is_installed,
                directory: installed.is_none() || is_installed,
                online_icon: false,
                icon: Default::default(),
            });
        }
        return Ok(rows);
    }

    let dir = instance_content_directory(game_dir, instance_id, kind)?;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let lower = file_name.to_ascii_lowercase();
        let include = match kind {
            2 => {
                file_type.is_file()
                    && (lower.ends_with(".jar")
                        || lower.ends_with(".litemod")
                        || lower.ends_with(".jar.disabled")
                        || lower.ends_with(".litemod.disabled"))
            }
            3 | 4 => file_type.is_dir() || lower.ends_with(".zip"),
            5 => file_type.is_dir(),
            _ => false,
        };
        if !include {
            continue;
        }
        let enabled = kind != 2 || !lower.ends_with(".disabled");
        let display_name = file_name
            .strip_suffix(".disabled")
            .unwrap_or(&file_name)
            .to_string();
        let detail = if file_type.is_dir() {
            "文件夹".to_string()
        } else {
            entry
                .metadata()
                .ok()
                .map(|meta| format!("{:.1} MiB", meta.len() as f64 / 1024.0 / 1024.0))
                .unwrap_or_else(|| "文件".to_string())
        };
        rows.push(InstanceContentRow {
            file_name: file_name.into(),
            name: display_name.into(),
            detail: detail.into(),
            enabled,
            directory: file_type.is_dir(),
            online_icon: false,
            icon: Default::default(),
        });
    }
    rows.sort_by_key(|row| row.name.to_lowercase());
    Ok(rows)
}

/// 打开 `saves/<folder_name>`。`direct_content_child` 顺带挡掉了 `..` 之类的
/// 路径穿越——世界的文件夹名是从 UI 传回来的字符串，不能当可信输入。
fn open_world(game_dir: &Path, instance_id: &str, folder_name: &str) -> Result<World, String> {
    let saves = instance_content_directory(game_dir, instance_id, 5)?;
    let path = direct_content_child(&saves, folder_name)?;
    World::open(&path).map_err(|e| e.to_string())
}

fn world_rows(game_dir: &Path, instance_id: &str) -> Vec<WorldRow> {
    let Ok(saves) = instance_content_directory(game_dir, instance_id, 5) else {
        return Vec::new();
    };
    World::list(&saves)
        .into_iter()
        .map(|world| {
            let icon = world
                .icon_path()
                .and_then(|path| slint::Image::load_from_path(&path).ok());
            WorldRow {
                folder_name: world.file_name().into(),
                name: match world.name() {
                    "" => world.file_name().to_string(),
                    name => hmcl_core::world::strip_formatting_codes(name),
                }
                .into(),
                game_version: world.game_version().unwrap_or_default().into(),
                last_played: hmcl_core::world::format_timestamp_millis(world.last_played()).into(),
                locked: world.is_locked(),
                supports_quick_play: world.supports_quick_play(),
                has_icon: icon.is_some(),
                icon: icon.unwrap_or_default(),
            }
        })
        .collect()
}

fn import_world(game_dir: &Path, instance_id: &str, archive: &Path) -> Result<String, String> {
    let saves = instance_content_directory(game_dir, instance_id, 5)?;
    let world = World::open(archive).map_err(|e| e.to_string())?;
    // 世界名是玩家随便起的，可能带 § 转义和文件名里不能用的字符。
    let cleaned = hmcl_core::world::strip_formatting_codes(world.name())
        .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
    let base = match cleaned.trim().trim_end_matches('.').trim() {
        "" => world.file_name().to_string(),
        name => name.to_string(),
    };
    let mut name = base.clone();
    let mut counter = 2;
    while saves.join(&name).exists() {
        name = format!("{base} ({counter})");
        counter += 1;
    }
    world.install(&saves, &name).map_err(|e| e.to_string())?;
    Ok(name)
}

fn selected_account(ui: &AppWindow) -> Option<KnownAccount> {
    let accounts = hmcl_core::settings::load::<AccountsFile>(
        &accounts_file_path(),
        hmcl_core::settings::accounts::SCHEMA_ID,
    )
    .value
    .known_accounts();
    let index = ui.get_selected_account_index();
    if index >= 0 {
        accounts.get(index as usize).cloned()
    } else {
        accounts.first().cloned()
    }
}

fn save_authlib_injector_server(
    server: hmcl_core::auth::authlib_injector::AuthlibInjectorServer,
) -> Result<(), String> {
    let path = authlib_injector_servers_file_path();
    let mut loaded = hmcl_core::settings::load::<AuthlibInjectorServersFile>(
        &path,
        AUTHLIB_INJECTOR_SERVERS_SCHEMA_ID,
    );
    if !loaded.can_save {
        return Err("认证服务器列表由较新版本 HMCL 创建，无法安全覆盖".to_string());
    }
    loaded.value.upsert(server);
    hmcl_core::settings::save(&path, AUTHLIB_INJECTOR_SERVERS_SCHEMA_ID, &loaded.value)
        .map_err(|error| format!("保存认证服务器失败: {error}"))
}

fn save_authlib_injector_account(
    login_name: &str,
    server: &hmcl_core::auth::authlib_injector::AuthlibInjectorServer,
    session: hmcl_core::auth::authlib_injector::AuthlibInjectorSession,
) -> Result<AuthlibInjectorAccountEntry, String> {
    let profile = session
        .selected_profile
        .as_ref()
        .ok_or_else(|| "认证服务器没有返回已选择的角色".to_string())?;
    let accounts_path = accounts_file_path();
    let mut accounts = hmcl_core::settings::load::<AccountsFile>(
        &accounts_path,
        hmcl_core::settings::accounts::SCHEMA_ID,
    );
    let mut entry = AuthlibInjectorAccountEntry::new(login_name, &server.url, profile);
    if let Some(existing) = accounts
        .value
        .authlib_injector_accounts()
        .into_iter()
        .find(|account| account.server_base_url == server.url && account.profile_id == profile.id)
    {
        entry.account_id = existing.account_id;
    }

    let tokens_path = authlib_injector_tokens_file_path();
    let mut tokens = hmcl_core::settings::load::<AuthlibInjectorAccountTokensFile>(
        &tokens_path,
        AUTHLIB_INJECTOR_TOKENS_SCHEMA_ID,
    );
    if !accounts.can_save || !tokens.can_save {
        return Err("账户文件由较新版本 HMCL 创建，无法安全覆盖".to_string());
    }
    tokens
        .value
        .accounts
        .insert(entry.account_id.clone(), session);
    hmcl_core::settings::save(
        &tokens_path,
        AUTHLIB_INJECTOR_TOKENS_SCHEMA_ID,
        &tokens.value,
    )
    .map_err(|error| format!("保存外置登录凭据失败: {error}"))?;
    accounts.value.upsert_authlib_injector_account(&entry);
    hmcl_core::settings::save(
        &accounts_path,
        hmcl_core::settings::accounts::SCHEMA_ID,
        &accounts.value,
    )
    .map_err(|error| format!("保存外置登录账户失败: {error}"))?;
    save_authlib_injector_server(server.clone())?;
    Ok(entry)
}

#[derive(Clone)]
struct PendingAuthlibLogin {
    server: hmcl_core::auth::authlib_injector::AuthlibInjectorServer,
    login_name: String,
    session: hmcl_core::auth::authlib_injector::AuthlibInjectorSession,
}

async fn finish_authlib_login_ui(
    ui_weak: slint::Weak<AppWindow>,
    entry: AuthlibInjectorAccountEntry,
) {
    set_selected_account(&entry.account_id);
    let name = entry.profile_name.clone();
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        refresh_accounts(&ui);
        refresh_authlib_injector_servers(&ui);
        restore_selected_account(&ui);
        ui.set_authlib_login_password("".into());
        ui.set_status_text(format!("外置登录账户 {name} 添加成功").into());
    });
    tokio::time::sleep(Duration::from_millis(350)).await;
    let _ = ui_weak.upgrade_in_event_loop(|ui| {
        ui.set_authlib_dialog_visible(false);
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = ui_weak.upgrade_in_event_loop(|ui| {
        ui.set_authlib_dialog_mounted(false);
    });
}

async fn account_auth(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    account: &KnownAccount,
) -> Result<launch::AuthInfo, String> {
    match account {
        KnownAccount::Offline(account) => {
            let uuid = account.resolved_profile_id();
            Ok(launch::AuthInfo {
                username: account.profile_name.clone(),
                uuid,
                access_token: uuid.simple().to_string(),
                user_type: launch::USER_TYPE_LEGACY.to_string(),
                user_properties: "{}".to_string(),
                launch_arguments: None,
            })
        }
        KnownAccount::Microsoft(account) => {
            let path = microsoft_tokens_file_path();
            let mut loaded = hmcl_core::settings::load::<MicrosoftAccountTokensFile>(
                &path,
                MICROSOFT_TOKENS_SCHEMA_ID,
            );
            let Some(mut session) = loaded.value.accounts.get(&account.account_id).cloned() else {
                return Err("微软账户的登录凭据不存在，请删除该账户后重新登录".to_string());
            };
            if session.needs_refresh() {
                session = hmcl_core::auth::microsoft::refresh(
                    client,
                    &hmcl_core::auth::microsoft::client_id(),
                    &session.refresh_token,
                )
                .await
                .map_err(|error| format!("刷新微软账户失败: {error}"))?;
                let _ = cache_microsoft_skin(client, &session).await;
                loaded
                    .value
                    .accounts
                    .insert(account.account_id.clone(), session.clone());
                if loaded.can_save {
                    hmcl_core::settings::save(&path, MICROSOFT_TOKENS_SCHEMA_ID, &loaded.value)
                        .map_err(|error| format!("保存微软账户失败: {error}"))?;
                }
            }
            Ok(session.auth_info())
        }
        KnownAccount::AuthlibInjector(account) => {
            let tokens_path = authlib_injector_tokens_file_path();
            let mut loaded = hmcl_core::settings::load::<AuthlibInjectorAccountTokensFile>(
                &tokens_path,
                AUTHLIB_INJECTOR_TOKENS_SCHEMA_ID,
            );
            let Some(mut session) = loaded.value.accounts.get(&account.account_id).cloned() else {
                return Err("外置登录账户的凭据不存在，请删除该账户后重新登录".to_string());
            };
            let artifact_path = authlib_injector_artifact_path();
            let (server, artifact) = tokio::try_join!(
                hmcl_core::auth::authlib_injector::locate_server(client, &account.server_base_url),
                hmcl_core::auth::authlib_injector::ensure_artifact(
                    client,
                    provider,
                    &artifact_path
                )
            )
            .map_err(|error| format!("准备外置登录失败: {error}"))?;
            if !hmcl_core::auth::authlib_injector::validate(client, &server, &session)
                .await
                .map_err(|error| format!("验证外置登录账户失败: {error}"))?
            {
                let profile = hmcl_core::auth::authlib_injector::GameProfile {
                    id: account.profile_id.clone(),
                    name: account.profile_name.clone(),
                };
                session = hmcl_core::auth::authlib_injector::refresh(
                    client,
                    &server,
                    &session,
                    Some(&profile),
                )
                .await
                .map_err(|error| format!("外置登录已过期，请重新登录: {error}"))?;
                loaded
                    .value
                    .accounts
                    .insert(account.account_id.clone(), session.clone());
                if loaded.can_save {
                    hmcl_core::settings::save(
                        &tokens_path,
                        AUTHLIB_INJECTOR_TOKENS_SCHEMA_ID,
                        &loaded.value,
                    )
                    .map_err(|error| format!("保存外置登录账户失败: {error}"))?;
                }
            }
            let _ = save_authlib_injector_server(server.clone());
            session
                .auth_info(&server, &artifact)
                .map_err(|error| format!("准备外置登录启动参数失败: {error}"))
        }
    }
}

fn set_worlds(ui: &AppWindow, game_dir: &Path) {
    ui.set_world_loading(true);
    let rows = world_rows(game_dir, ui.get_settings_instance_id().as_str());
    ui.set_world_loading(false);
    ui.set_world_items(slint::ModelRc::new(slint::VecModel::from(rows)));
}

fn format_location(location: Option<hmcl_core::world::Location>) -> String {
    location
        .map(|location| {
            let position = format!("({:.1}, {:.1}, {:.1})", location.x, location.y, location.z);
            match location.dimension {
                Some(dimension) => format!("{dimension} {position}"),
                None => position,
            }
        })
        .unwrap_or_default()
}

fn format_play_time(ticks: Option<i64>) -> String {
    let Some(ticks) = ticks else {
        return String::new();
    };
    let minutes = ticks / 20 / 60;
    format!(
        "{} 天 {} 小时 {} 分钟",
        minutes / 1440,
        minutes % 1440 / 60,
        minutes % 60
    )
}

fn blurred_seed_image(seed: &str) -> slint::Image {
    if seed.is_empty() {
        return slint::Image::default();
    }
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="150" height="30" viewBox="0 0 150 30">
<defs><filter id="blur" x="-10%" y="-40%" width="120%" height="180%"><feGaussianBlur stdDeviation="2.2"/></filter></defs>
<text x="148" y="20" text-anchor="end" font-family="sans-serif" font-size="12" fill="#55535c" filter="url(#blur)">{seed}</text>
</svg>"##
    );
    slint::Image::load_from_svg_data(svg.as_bytes()).unwrap_or_default()
}

/// 把一个世界的全部信息灌进详情页的属性里。`world-detail-folder` 必须已经设好。
fn load_world_detail(ui: &AppWindow, game_dir: &Path) {
    let instance_id = ui.get_settings_instance_id().to_string();
    let folder = ui.get_world_detail_folder().to_string();
    let Ok(world) = open_world(game_dir, &instance_id, &folder) else {
        ui.set_status_text("世界加载失败".into());
        return;
    };

    let icon = world
        .icon_path()
        .and_then(|path| slint::Image::load_from_path(&path).ok());
    ui.set_world_detail_has_icon(icon.is_some());
    ui.set_world_detail_icon(icon.unwrap_or_default());
    ui.set_world_detail_name(hmcl_core::world::strip_formatting_codes(world.name()).into());
    ui.set_world_detail_version(world.game_version().unwrap_or_default().into());
    let seed = world.seed().map(|s| s.to_string()).unwrap_or_default();
    ui.set_world_detail_seed_blurred(blurred_seed_image(&seed));
    ui.set_world_detail_seed(seed.into());
    ui.set_world_detail_spawn(format_location(world.spawn_point()).into());
    ui.set_world_detail_last_played(
        hmcl_core::world::format_timestamp_millis(world.last_played()).into(),
    );
    ui.set_world_detail_play_time(format_play_time(world.play_time_ticks()).into());
    ui.set_world_detail_quick_play(world.supports_quick_play());
    ui.set_world_detail_data_packs(world.supports_data_packs());
    // 世界被游戏占用时整页只读。原版是进页面就抢住 session.lock 一直握着，我们
    // 只在每次读写前后查一下——GUI 这边没有"页面生命周期"能挂住一个文件句柄。
    ui.set_world_detail_read_only(world.is_locked());

    ui.set_world_allow_commands_available(world.allow_commands().is_some());
    ui.set_world_allow_commands(world.allow_commands().unwrap_or(false));
    ui.set_world_generate_features_available(world.generate_features().is_some());
    ui.set_world_generate_features(world.generate_features().unwrap_or(false));
    ui.set_world_difficulty_available(world.difficulty().is_some());
    ui.set_world_difficulty_index(world.difficulty().map(|d| d.index() as i32).unwrap_or(0));
    ui.set_world_difficulty_locked_available(world.difficulty_locked().is_some());
    ui.set_world_difficulty_locked(world.difficulty_locked().unwrap_or(false));

    ui.set_world_has_player(world.has_player_data());
    ui.set_world_player_location(format_location(world.player_location()).into());
    ui.set_world_player_death(format_location(world.last_death_location()).into());
    ui.set_world_player_respawn(format_location(world.player_respawn()).into());
    ui.set_world_game_type_available(world.game_type().is_some());
    ui.set_world_game_type_index(world.game_type().map(|g| g.index() as i32).unwrap_or(0));
    ui.set_world_player_health(
        world
            .player_health()
            .map(|v| format!("{v}"))
            .unwrap_or_default()
            .into(),
    );
    ui.set_world_player_food(
        world
            .player_food_level()
            .map(|v| v.to_string())
            .unwrap_or_default()
            .into(),
    );
    ui.set_world_player_saturation(
        world
            .player_food_saturation()
            .map(|v| format!("{v}"))
            .unwrap_or_default()
            .into(),
    );
    ui.set_world_player_xp(
        world
            .player_xp_level()
            .map(|v| v.to_string())
            .unwrap_or_default()
            .into(),
    );
}

fn edit_world(
    ui: &AppWindow,
    game_dir: &Path,
    edit: impl FnOnce(&mut World) -> Result<(), hmcl_core::world::WorldError>,
) {
    let instance_id = ui.get_settings_instance_id().to_string();
    let folder = ui.get_world_detail_folder().to_string();
    let result = open_world(game_dir, &instance_id, &folder)
        .and_then(|mut world| edit(&mut world).map_err(|e| e.to_string()));
    match result {
        Ok(()) => ui.set_status_text("已保存世界设置".into()),
        Err(e) => ui.set_status_text(format!("保存世界设置失败: {e}").into()),
    }
}

fn instance_backups_directory(game_dir: &Path, instance_id: &str) -> PathBuf {
    GameRepository::new(game_dir)
        .run_directory(instance_id)
        .join("backups")
}

fn set_world_backups(ui: &AppWindow, game_dir: &Path) {
    let instance_id = ui.get_settings_instance_id().to_string();
    let folder = ui.get_world_detail_folder().to_string();
    ui.set_world_backup_delete_index(-1);
    let Ok(world) = open_world(game_dir, &instance_id, &folder) else {
        return;
    };
    let rows: Vec<BackupRow> = world
        .backups(&instance_backups_directory(game_dir, &instance_id))
        .into_iter()
        .map(|backup| {
            let inner = World::open(&backup.path).ok();
            BackupRow {
                file_name: backup
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
                world_name: inner
                    .as_ref()
                    .map(|world| hmcl_core::world::strip_formatting_codes(world.name()))
                    .unwrap_or_default()
                    .into(),
                game_version: inner
                    .as_ref()
                    .and_then(|world| world.game_version())
                    .unwrap_or_default()
                    .into(),
                time: match backup.count {
                    0 => backup.time.clone(),
                    count => format!("{} ({count})", backup.time),
                }
                .into(),
            }
        })
        .collect();
    ui.set_world_backups(slint::ModelRc::new(slint::VecModel::from(rows)));
}

fn world_datapacks_directory(
    game_dir: &Path,
    instance_id: &str,
    folder: &str,
) -> Result<PathBuf, String> {
    let saves = instance_content_directory(game_dir, instance_id, 5)?;
    Ok(hmcl_core::datapack::directory_of(&direct_content_child(
        &saves, folder,
    )?))
}

fn set_world_datapacks(ui: &AppWindow, game_dir: &Path) {
    let instance_id = ui.get_settings_instance_id().to_string();
    let folder = ui.get_world_detail_folder().to_string();
    ui.set_world_datapack_delete_index(-1);
    let Ok(dir) = world_datapacks_directory(game_dir, &instance_id, &folder) else {
        return;
    };
    let rows: Vec<DataPackRow> = hmcl_core::datapack::list(&dir)
        .into_iter()
        .map(|pack| DataPackRow {
            file_name: pack
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
            name: pack.id.into(),
            description: pack.description.into(),
            enabled: pack.enabled,
        })
        .collect();
    ui.set_world_datapacks(slint::ModelRc::new(slint::VecModel::from(rows)));
}

fn set_instance_content(ui: &AppWindow, game_dir: &Path, kind: i32) {
    if kind == 5 {
        set_worlds(ui, game_dir);
        return;
    }
    ui.set_instance_content_loading(true);
    if kind == 1 {
        let repo = GameRepository::new(game_dir);
        let versions = repo.load_all_versions();
        if let Some(instance) = versions.get(ui.get_settings_instance_id().as_str()) {
            if let Ok(resolved) = instance.resolve(&versions) {
                ui.set_instance_game_version(
                    game_install::game_version_of(instance, &resolved).into(),
                );
            }
        }
    }
    let rows = local_content_rows(game_dir, ui.get_settings_instance_id().as_str(), kind);
    ui.set_instance_content_loading(false);
    ui.set_instance_content_delete_confirm_index(-1);
    match rows {
        Ok(rows) => ui.set_instance_content_items(slint::ModelRc::new(slint::VecModel::from(rows))),
        Err(e) => {
            ui.set_instance_content_items(slint::ModelRc::new(slint::VecModel::from(Vec::<
                InstanceContentRow,
            >::new(
            ))));
            ui.set_status_text(format!("刷新实例内容失败: {e}").into());
        }
    }
}

fn direct_content_child(root: &Path, file_name: &str) -> Result<PathBuf, String> {
    let path = Path::new(file_name);
    if path.components().count() != 1
        || path.file_name().and_then(|name| name.to_str()) != Some(file_name)
    {
        return Err("无效的文件名".to_string());
    }
    Ok(root.join(path))
}

fn toggle_instance_mod(
    game_dir: &Path,
    instance_id: &str,
    file_name: &str,
) -> Result<String, String> {
    let root = instance_content_directory(game_dir, instance_id, 2)?;
    let source = direct_content_child(&root, file_name)?;
    let target_name = file_name
        .strip_suffix(".disabled")
        .map(str::to_string)
        .unwrap_or_else(|| format!("{file_name}.disabled"));
    let target = direct_content_child(&root, &target_name)?;
    if target.exists() {
        return Err(format!("目标文件已存在: {target_name}"));
    }
    std::fs::rename(source, target)
        .map(|_| target_name)
        .map_err(|e| e.to_string())
}

fn delete_instance_content(
    game_dir: &Path,
    instance_id: &str,
    kind: i32,
    file_name: &str,
) -> Result<(), String> {
    let root = instance_content_directory(game_dir, instance_id, kind)?;
    let target = direct_content_child(&root, file_name)?;
    let metadata = std::fs::symlink_metadata(&target).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(target).map_err(|e| e.to_string())
    } else if metadata.is_dir() {
        std::fs::remove_dir_all(target).map_err(|e| e.to_string())
    } else {
        Err("不支持删除该类型的文件".to_string())
    }
}

fn install_local_instance_content(
    game_dir: &Path,
    instance_id: &str,
    kind: i32,
    sources: &[PathBuf],
) -> Result<usize, String> {
    let destination = instance_content_directory(game_dir, instance_id, kind)?;
    std::fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
    let mut copies = Vec::new();
    for source in sources {
        let file_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("无效的文件名: {}", source.display()))?;
        let lower = file_name.to_ascii_lowercase();
        let valid = match kind {
            2 => lower.ends_with(".jar") || lower.ends_with(".litemod"),
            3 | 4 => lower.ends_with(".zip"),
            _ => false,
        };
        if !valid {
            return Err(format!("不支持的文件: {file_name}"));
        }
        let target = direct_content_child(&destination, file_name)?;
        if target.exists() {
            return Err(format!("{file_name} 已存在"));
        }
        copies.push((source, target));
    }
    for (source, target) in &copies {
        std::fs::copy(source, target).map_err(|e| e.to_string())?;
    }
    Ok(copies.len())
}

async fn instance_content_rows_online(
    game_dir: &Path,
    instance_id: &str,
    kind: i32,
) -> Result<(Vec<PendingInstanceContentRow>, usize), String> {
    instance_content_rows_online_inner(game_dir, instance_id, kind, true).await
}

#[derive(Clone)]
struct PendingInstanceContentRow {
    file_name: String,
    name: String,
    detail: String,
    enabled: bool,
    directory: bool,
    icon_path: Option<PathBuf>,
}

type InstanceContentCache = Arc<Mutex<HashMap<(String, i32), Vec<PendingInstanceContentRow>>>>;

fn cached_content_matches_local(
    cached: &[PendingInstanceContentRow],
    local: &[InstanceContentRow],
) -> bool {
    cached.len() == local.len()
        && cached.iter().zip(local).all(|(cached, local)| {
            cached.file_name == local.file_name.as_str()
                && cached.enabled == local.enabled
                && cached.directory == local.directory
                && cached.detail == local.detail.as_str()
        })
}

async fn instance_content_rows_online_inner(
    game_dir: &Path,
    instance_id: &str,
    kind: i32,
    check_updates: bool,
) -> Result<(Vec<PendingInstanceContentRow>, usize), String> {
    let rows = local_content_rows(game_dir, instance_id, kind)?
        .into_iter()
        .map(|row| PendingInstanceContentRow {
            file_name: row.file_name.to_string(),
            name: row.name.to_string(),
            detail: row.detail.to_string(),
            enabled: row.enabled,
            directory: row.directory,
            icon_path: None,
        })
        .collect::<Vec<_>>();
    let context = resolve_instance_context(game_dir, instance_id);
    let game_version = context.as_ref().map(|(version, _)| version.clone());
    let loader = if kind == 2 {
        context.as_ref().and_then(|(_, loader)| *loader)
    } else {
        None
    };
    let client = http_client();
    let provider = Arc::new(configured_download_provider(false));
    let concurrency = provider.concurrency();
    let directory = instance_content_directory(game_dir, instance_id, kind)?;
    let icon_dir = game_dir.join(".hmcl-rs-cache").join("project-icons");
    let resolved = stream::iter(rows.into_iter().map(|mut row| {
        let client = client.clone();
        let provider = provider.clone();
        let directory = directory.clone();
        let icon_dir = icon_dir.clone();
        let game_version = game_version.clone();
        async move {
            if row.directory {
                return (row, false);
            }
            let original_detail = row.detail.to_string();
            let path = directory.join(&row.file_name);
            let current = async {
                let hash = hmcl_core::download::fetch::sha1_file(&path)
                    .await
                    .map_err(|e| e.to_string())?;
                modrinth::fetch_version_by_sha1(&client, &provider, &hash)
                    .await
                    .map_err(|e| e.to_string())?
                    .filter(|version| !version.project_id.is_empty())
                    .ok_or_else(|| "未在 Modrinth 中找到".to_string())
            }
            .await;
            let Ok(current) = current else {
                if check_updates {
                    row.detail = format!("{original_detail} · {}", current.unwrap_err());
                }
                return (row, false);
            };

            let icon_path = match modrinth::fetch_project(&client, &provider, &current.project_id)
                .await
            {
                Ok(project) => {
                    load_project_icon(&client, &icon_dir, &project.id, project.icon_url.as_deref())
                        .await
                }
                Err(_) => None,
            };
            if !check_updates {
                row.icon_path = icon_path;
                return (row, false);
            }

            let latest = modrinth::fetch_project_versions(
                &client,
                &provider,
                &current.project_id,
                game_version.as_deref(),
                loader,
            )
            .await
            .map_err(|e| e.to_string())
            .and_then(|versions| {
                versions
                    .into_iter()
                    .find(|version| !version.files.is_empty())
                    .ok_or_else(|| "没有兼容的在线版本".to_string())
            });
            match latest {
                Ok(latest) if current.id == latest.id => {
                    row.detail = format!("{original_detail} · 已是最新版本");
                    row.icon_path = icon_path;
                    (row, false)
                }
                Ok(latest) => {
                    row.detail = format!("{original_detail} · 可更新至 {}", latest.version_number);
                    row.icon_path = icon_path;
                    (row, true)
                }
                Err(message) => {
                    row.detail = format!("{original_detail} · {message}");
                    row.icon_path = icon_path;
                    (row, false)
                }
            }
        }
    }))
    .buffered(concurrency)
    .collect::<Vec<_>>()
    .await;
    let update_count = resolved.iter().filter(|(_, update)| *update).count();
    Ok((
        resolved.into_iter().map(|(row, _)| row).collect(),
        update_count,
    ))
}

fn materialize_instance_content_rows(
    rows: Vec<PendingInstanceContentRow>,
) -> Vec<InstanceContentRow> {
    rows.into_iter()
        .map(|row| {
            let icon = row
                .icon_path
                .as_deref()
                .and_then(|path| slint::Image::load_from_path(path).ok())
                .unwrap_or_default();
            InstanceContentRow {
                file_name: row.file_name.into(),
                name: row.name.into(),
                detail: row.detail.into(),
                enabled: row.enabled,
                directory: row.directory,
                online_icon: row.icon_path.is_some(),
                icon,
            }
        })
        .collect()
}

fn set_status(ui_weak: &slint::Weak<AppWindow>, text: String) {
    let ui_weak = ui_weak.clone();
    let _ = ui_weak.upgrade_in_event_loop(move |ui| ui.set_status_text(text.into()));
}

fn retain_recent_game_log(logs: &Arc<Mutex<VecDeque<String>>>, line: &str) {
    const MAX_LINES: usize = 600;
    let mut logs = logs.lock().unwrap();
    if logs.len() == MAX_LINES {
        logs.pop_front();
    }
    logs.push_back(line.to_string());
}

fn recent_game_log_text(logs: &Arc<Mutex<VecDeque<String>>>, latest_log: &Path) -> String {
    let captured = logs
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    if !captured.is_empty() {
        return captured;
    }
    let Ok(contents) = std::fs::read_to_string(latest_log) else {
        return String::new();
    };
    let mut lines = contents.lines().rev().take(600).collect::<Vec<_>>();
    lines.reverse();
    lines.join("\n")
}

fn crash_log_level(line: &str) -> i32 {
    let upper = line.to_ascii_uppercase();
    if upper.contains("/ERROR]")
        || upper.contains("[ERROR]")
        || upper.contains("/FATAL]")
        || upper.contains("[FATAL]")
        || upper.contains("EXCEPTION")
        || upper.contains("CAUSED BY:")
        || upper.contains("PANIC")
    {
        4
    } else if upper.contains("/WARN]") || upper.contains("[WARN]") {
        3
    } else if upper.contains("/INFO]") || upper.contains("[INFO]") {
        2
    } else if upper.contains("/DEBUG]")
        || upper.contains("[DEBUG]")
        || upper.contains("/TRACE]")
        || upper.contains("[TRACE]")
    {
        1
    } else {
        0
    }
}

fn push_launch_progress(ui_weak: &slint::Weak<AppWindow>, active: usize, detail: String) {
    let labels = [
        "检查并补全游戏文件",
        "检测 Java",
        "生成启动参数",
        "启动游戏进程",
    ];
    let rows = labels
        .into_iter()
        .enumerate()
        .map(|(index, label)| InstallStageRow {
            label: label.into(),
            done: 0,
            total: 0,
            state: if index < active {
                2
            } else if index == active {
                1
            } else {
                0
            },
            show_count: false,
        })
        .collect::<Vec<_>>();
    let ui_weak = ui_weak.clone();
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.set_launch_stage_lines(slint::ModelRc::new(slint::VecModel::from(rows)));
        ui.set_launch_progress_detail(detail.into());
    });
}

fn populate_instance_settings_ui(
    ui: &AppWindow,
    s: &hmcl_core::settings::instance_game_settings::InstanceGameSettings,
    global: Option<&GlobalGameSettingsPreset>,
) {
    use hmcl_core::settings::instance_game_settings::*;

    ui.set_settings_syncing(true);
    ui.set_java_type_overridden(s.is_overridden(PROPERTY_JAVA_TYPE));
    let java_type = if s.is_overridden(PROPERTY_JAVA_TYPE) {
        s.java_type
    } else {
        match global.and_then(|preset| preset.java_type.as_deref()) {
            Some("VERSION") => Some(JavaSelectionType::Version),
            Some("CUSTOM") => Some(JavaSelectionType::Custom),
            _ => Some(JavaSelectionType::Auto),
        }
    };
    ui.set_java_type_index(match java_type {
        Some(JavaSelectionType::Version) => 1,
        Some(JavaSelectionType::Custom) => 2,
        _ => 0,
    });
    ui.set_custom_java_version_overridden(s.is_overridden(PROPERTY_CUSTOM_JAVA_VERSION));
    ui.set_custom_java_version(
        if s.is_overridden(PROPERTY_CUSTOM_JAVA_VERSION) {
            s.custom_java_version.clone()
        } else {
            global.and_then(|preset| preset.custom_java_version.clone())
        }
        .unwrap_or_default()
        .into(),
    );
    ui.set_custom_java_path_overridden(s.is_overridden(PROPERTY_CUSTOM_JAVA_PATH));
    ui.set_custom_java_path(
        if s.is_overridden(PROPERTY_CUSTOM_JAVA_PATH) {
            s.custom_java_path.clone()
        } else {
            global.and_then(|preset| preset.custom_java_path.clone())
        }
        .unwrap_or_default()
        .into(),
    );
    ui.set_jvm_options_overridden(s.is_overridden(PROPERTY_JVM_OPTIONS));
    ui.set_jvm_options(
        if s.is_overridden(PROPERTY_JVM_OPTIONS) {
            s.jvm_options.clone()
        } else {
            global.and_then(|preset| preset.jvm_options.clone())
        }
        .unwrap_or_default()
        .into(),
    );
    ui.set_no_jvm_options_overridden(s.is_overridden(PROPERTY_NO_JVM_OPTIONS));
    ui.set_no_jvm_options(if s.is_overridden(PROPERTY_NO_JVM_OPTIONS) {
        s.no_jvm_options.unwrap_or(false)
    } else {
        global
            .and_then(|preset| preset.no_jvm_options)
            .unwrap_or(false)
    });
    ui.set_no_optimizing_jvm_options_overridden(
        s.is_overridden(PROPERTY_NO_OPTIMIZING_JVM_OPTIONS),
    );
    ui.set_no_optimizing_jvm_options(if s.is_overridden(PROPERTY_NO_OPTIMIZING_JVM_OPTIONS) {
        s.no_optimizing_jvm_options.unwrap_or(false)
    } else {
        global
            .and_then(|preset| preset.no_optimizing_jvm_options)
            .unwrap_or(false)
    });
    ui.set_not_check_jvm_overridden(s.is_overridden(PROPERTY_NOT_CHECK_JVM));
    ui.set_not_check_jvm(s.not_check_jvm.unwrap_or(false));

    ui.set_auto_memory_overridden(s.is_overridden(PROPERTY_AUTO_MEMORY));
    ui.set_auto_memory(if s.is_overridden(PROPERTY_AUTO_MEMORY) {
        s.auto_memory.unwrap_or(true)
    } else {
        global.and_then(|preset| preset.auto_memory).unwrap_or(true)
    });
    ui.set_max_memory_overridden(s.is_overridden(PROPERTY_MAX_MEMORY));
    ui.set_max_memory(
        if s.is_overridden(PROPERTY_MAX_MEMORY) {
            s.max_memory
        } else {
            global.and_then(|preset| preset.max_memory)
        }
        .unwrap_or(2048)
        .to_string()
        .into(),
    );
    ui.set_min_memory_overridden(s.is_overridden(PROPERTY_MIN_MEMORY));
    ui.set_min_memory(
        if s.is_overridden(PROPERTY_MIN_MEMORY) {
            s.min_memory
        } else {
            global.and_then(|preset| preset.min_memory)
        }
        .map(|v| v.to_string())
        .unwrap_or_default()
        .into(),
    );
    ui.set_perm_size_overridden(s.is_overridden(PROPERTY_PERM_SIZE));
    ui.set_perm_size(
        if s.is_overridden(PROPERTY_PERM_SIZE) {
            s.permanent_generation_size
        } else {
            global.and_then(|preset| preset.perm_size)
        }
        .map(|v| v.to_string())
        .unwrap_or_default()
        .into(),
    );

    ui.set_window_type_overridden(s.is_overridden(PROPERTY_WINDOW_TYPE));
    ui.set_window_type_index(
        match s
            .window_type
            .map(|value| match value {
                WindowType::Fullscreen => "FULLSCREEN",
                WindowType::Maximized => "MAXIMIZED",
                WindowType::Windowed => "WINDOWED",
            })
            .or_else(|| global.and_then(|preset| preset.window_type.as_deref()))
        {
            Some("FULLSCREEN") => 1,
            Some("MAXIMIZED") => 2,
            _ => 0,
        },
    );
    ui.set_width_overridden(s.is_overridden(PROPERTY_WIDTH));
    ui.set_settings_width(
        if s.is_overridden(PROPERTY_WIDTH) {
            s.width
        } else {
            global.and_then(|preset| preset.width)
        }
        .unwrap_or(1280.0)
        .round()
        .to_string()
        .into(),
    );
    ui.set_height_overridden(s.is_overridden(PROPERTY_HEIGHT));
    ui.set_settings_height(
        if s.is_overridden(PROPERTY_HEIGHT) {
            s.height
        } else {
            global.and_then(|preset| preset.height)
        }
        .unwrap_or(720.0)
        .round()
        .to_string()
        .into(),
    );

    ui.set_quick_play_overridden(s.is_overridden(PROPERTY_QUICK_PLAY));
    ui.set_quick_play_index(
        match s
            .quick_play
            .map(|value| match value {
                QuickPlayType::Multiplayer => "MULTIPLAYER",
                QuickPlayType::Singleplayer => "SINGLEPLAYER",
                QuickPlayType::Realms => "REALMS",
                QuickPlayType::None => "NONE",
            })
            .or_else(|| global.and_then(|preset| preset.quick_play.as_deref()))
        {
            Some("MULTIPLAYER") => 1,
            Some("SINGLEPLAYER") => 2,
            Some("REALMS") => 3,
            _ => 0,
        },
    );
    ui.set_quick_play_multiplayer(
        if s.is_overridden(PROPERTY_QUICK_PLAY_MULTIPLAYER) {
            s.quick_play_multiplayer.clone()
        } else {
            global.and_then(|preset| preset.quick_play_multiplayer.clone())
        }
        .unwrap_or_default()
        .into(),
    );
    ui.set_quick_play_singleplayer(
        if s.is_overridden(PROPERTY_QUICK_PLAY_SINGLEPLAYER) {
            s.quick_play_singleplayer.clone()
        } else {
            global.and_then(|preset| preset.quick_play_singleplayer.clone())
        }
        .unwrap_or_default()
        .into(),
    );
    ui.set_quick_play_realms(
        if s.is_overridden(PROPERTY_QUICK_PLAY_REALMS) {
            s.quick_play_realms.clone()
        } else {
            global.and_then(|preset| preset.quick_play_realms.clone())
        }
        .unwrap_or_default()
        .into(),
    );

    ui.set_launcher_visibility_overridden(s.is_overridden(PROPERTY_LAUNCHER_VISIBILITY));
    let launcher_visibility = if s.is_overridden(PROPERTY_LAUNCHER_VISIBILITY) {
        s.launcher_visibility.map(|value| match value {
            LauncherVisibility::Close => "CLOSE",
            LauncherVisibility::Hide => "HIDE",
            LauncherVisibility::Keep => "KEEP",
            LauncherVisibility::HideAndReopen => "HIDE_AND_REOPEN",
        })
    } else {
        global.and_then(|preset| preset.launcher_visibility.as_deref())
    };
    ui.set_launcher_visibility_index(match launcher_visibility {
        Some("CLOSE") => 0,
        Some("HIDE") => 1,
        Some("HIDE_AND_REOPEN") => 3,
        _ => 2,
    });
    ui.set_debug_log_overridden(s.is_overridden(PROPERTY_ENABLE_DEBUG_LOG_OUTPUT));
    ui.set_debug_log(if s.is_overridden(PROPERTY_ENABLE_DEBUG_LOG_OUTPUT) {
        s.enable_debug_log_output.unwrap_or(false)
    } else {
        global
            .and_then(|preset| preset.enable_debug_log_output)
            .unwrap_or(false)
    });

    ui.set_running_directory_overridden(s.is_overridden(PROPERTY_RUNNING_DIRECTORY));
    ui.set_running_directory(s.running_directory.clone().unwrap_or_default().into());
    ui.set_game_arguments_overridden(s.is_overridden(PROPERTY_GAME_ARGUMENTS));
    ui.set_game_arguments(
        if s.is_overridden(PROPERTY_GAME_ARGUMENTS) {
            s.game_arguments.clone()
        } else {
            global.and_then(|preset| preset.game_arguments.clone())
        }
        .unwrap_or_default()
        .into(),
    );
    ui.set_environment_variables_overridden(s.is_overridden(PROPERTY_ENVIRONMENT_VARIABLES));
    ui.set_environment_variables(
        if s.is_overridden(PROPERTY_ENVIRONMENT_VARIABLES) {
            s.environment_variables.clone()
        } else {
            global.and_then(|preset| preset.environment_variables.clone())
        }
        .unwrap_or_default()
        .into(),
    );
    ui.set_command_wrapper_overridden(s.is_overridden(PROPERTY_COMMAND_WRAPPER));
    ui.set_command_wrapper(
        if s.is_overridden(PROPERTY_COMMAND_WRAPPER) {
            s.command_wrapper.clone()
        } else {
            global.and_then(|preset| preset.command_wrapper.clone())
        }
        .unwrap_or_default()
        .into(),
    );
    ui.set_pre_launch_command_overridden(s.is_overridden(PROPERTY_PRE_LAUNCH_COMMAND));
    ui.set_pre_launch_command(
        if s.is_overridden(PROPERTY_PRE_LAUNCH_COMMAND) {
            s.pre_launch_command.clone()
        } else {
            global.and_then(|preset| preset.pre_launch_command.clone())
        }
        .unwrap_or_default()
        .into(),
    );
    ui.set_post_exit_command_overridden(s.is_overridden(PROPERTY_POST_EXIT_COMMAND));
    ui.set_post_exit_command(
        if s.is_overridden(PROPERTY_POST_EXIT_COMMAND) {
            s.post_exit_command.clone()
        } else {
            global.and_then(|preset| preset.post_exit_command.clone())
        }
        .unwrap_or_default()
        .into(),
    );
    ui.set_process_priority_overridden(s.is_overridden(PROPERTY_PROCESS_PRIORITY));
    ui.set_process_priority_index(
        match s
            .process_priority
            .map(|value| match value {
                ProcessPriority::Low => "LOW",
                ProcessPriority::BelowNormal => "BELOW_NORMAL",
                ProcessPriority::Normal => "NORMAL",
                ProcessPriority::AboveNormal => "ABOVE_NORMAL",
                ProcessPriority::High => "HIGH",
            })
            .or_else(|| global.and_then(|preset| preset.process_priority.as_deref()))
        {
            Some("LOW") => 0,
            Some("BELOW_NORMAL") => 1,
            Some("ABOVE_NORMAL") => 3,
            Some("HIGH") => 4,
            _ => 2,
        },
    );
    ui.set_graphics_backend_overridden(s.is_overridden(PROPERTY_GRAPHICS_BACKEND));
    ui.set_graphics_backend_index(
        match s
            .graphics_backend
            .map(|value| match value {
                hmcl_core::launch::GraphicsApi::OpenGl => "OPENGL",
                hmcl_core::launch::GraphicsApi::Vulkan => "VULKAN",
                hmcl_core::launch::GraphicsApi::Default => "DEFAULT",
            })
            .or_else(|| global.and_then(|preset| preset.graphics_backend.as_deref()))
        {
            Some("OPENGL") => 1,
            Some("VULKAN") => 2,
            _ => 0,
        },
    );
    ui.set_use_custom_natives_overridden(s.is_overridden(PROPERTY_USE_CUSTOM_NATIVES));
    ui.set_use_custom_natives(if s.is_overridden(PROPERTY_USE_CUSTOM_NATIVES) {
        s.use_custom_natives.unwrap_or(false)
    } else {
        global
            .and_then(|preset| preset.use_custom_natives)
            .unwrap_or(false)
    });
    ui.set_natives_directory_overridden(s.is_overridden(PROPERTY_NATIVES_DIRECTORY));
    ui.set_natives_directory(
        if s.is_overridden(PROPERTY_NATIVES_DIRECTORY) {
            s.natives_directory.clone()
        } else {
            global.and_then(|preset| preset.natives_directory.clone())
        }
        .unwrap_or_default()
        .into(),
    );
    ui.set_settings_syncing(false);
}

/// 跟 [`populate_instance_settings_ui`] 反过来: 把 UI 属性写回一份已经从磁盘读出来
/// 的 `InstanceGameSettings`（不是从零构造一个新的——这样 `extra`/`parent`/`icon`
/// 这些 UI 没管的字段不会被冲掉）。`override_properties` 只清掉这个 UI 认识
/// 的那些属性名再按当前勾选重新加, 不认识的属性名(不管是旧版本 HMCL 写的还是
/// 真实 HMCL 写的)原样保留。
fn apply_ui_to_instance_settings(
    ui: &AppWindow,
    s: &mut hmcl_core::settings::instance_game_settings::InstanceGameSettings,
) {
    use hmcl_core::settings::instance_game_settings::*;

    const MANAGED: &[&str] = &[
        PROPERTY_JAVA_TYPE,
        PROPERTY_CUSTOM_JAVA_VERSION,
        PROPERTY_CUSTOM_JAVA_PATH,
        PROPERTY_JVM_OPTIONS,
        PROPERTY_NO_JVM_OPTIONS,
        PROPERTY_NO_OPTIMIZING_JVM_OPTIONS,
        PROPERTY_NOT_CHECK_JVM,
        PROPERTY_AUTO_MEMORY,
        PROPERTY_MAX_MEMORY,
        PROPERTY_MIN_MEMORY,
        PROPERTY_PERM_SIZE,
        PROPERTY_WINDOW_TYPE,
        PROPERTY_WIDTH,
        PROPERTY_HEIGHT,
        PROPERTY_QUICK_PLAY,
        PROPERTY_QUICK_PLAY_MULTIPLAYER,
        PROPERTY_QUICK_PLAY_SINGLEPLAYER,
        PROPERTY_QUICK_PLAY_REALMS,
        PROPERTY_LAUNCHER_VISIBILITY,
        PROPERTY_ENABLE_DEBUG_LOG_OUTPUT,
        PROPERTY_RUNNING_DIRECTORY,
        PROPERTY_GAME_ARGUMENTS,
        PROPERTY_GRAPHICS_BACKEND,
        PROPERTY_ENVIRONMENT_VARIABLES,
        PROPERTY_COMMAND_WRAPPER,
        PROPERTY_PRE_LAUNCH_COMMAND,
        PROPERTY_POST_EXIT_COMMAND,
        PROPERTY_PROCESS_PRIORITY,
        PROPERTY_USE_CUSTOM_NATIVES,
        PROPERTY_NATIVES_DIRECTORY,
    ];
    s.override_properties
        .retain(|p| !MANAGED.contains(&p.as_str()));

    s.java_type = Some(match ui.get_java_type_index() {
        1 => JavaSelectionType::Version,
        2 => JavaSelectionType::Custom,
        _ => JavaSelectionType::Auto,
    });
    s.custom_java_version = Some(ui.get_custom_java_version().to_string());
    s.custom_java_path = Some(ui.get_custom_java_path().to_string());
    s.jvm_options = Some(ui.get_jvm_options().to_string());
    s.no_jvm_options = Some(ui.get_no_jvm_options());
    s.no_optimizing_jvm_options = Some(ui.get_no_optimizing_jvm_options());
    s.not_check_jvm = Some(ui.get_not_check_jvm());
    s.auto_memory = Some(ui.get_auto_memory());
    s.max_memory = ui.get_max_memory().parse().ok();
    s.min_memory = ui.get_min_memory().parse().ok();
    s.permanent_generation_size = ui.get_perm_size().parse().ok();
    s.window_type = Some(match ui.get_window_type_index() {
        1 => WindowType::Fullscreen,
        2 => WindowType::Maximized,
        _ => WindowType::Windowed,
    });
    s.width = ui.get_settings_width().parse().ok();
    s.height = ui.get_settings_height().parse().ok();
    s.quick_play = Some(match ui.get_quick_play_index() {
        1 => QuickPlayType::Multiplayer,
        2 => QuickPlayType::Singleplayer,
        3 => QuickPlayType::Realms,
        _ => QuickPlayType::None,
    });
    s.quick_play_multiplayer = Some(ui.get_quick_play_multiplayer().to_string());
    s.quick_play_singleplayer = Some(ui.get_quick_play_singleplayer().to_string());
    s.quick_play_realms = Some(ui.get_quick_play_realms().to_string());
    s.launcher_visibility = Some(match ui.get_launcher_visibility_index() {
        0 => LauncherVisibility::Close,
        2 => LauncherVisibility::Keep,
        3 => LauncherVisibility::HideAndReopen,
        _ => LauncherVisibility::Hide,
    });
    s.enable_debug_log_output = Some(ui.get_debug_log());
    s.running_directory = Some(ui.get_running_directory().to_string());
    s.game_arguments = Some(ui.get_game_arguments().to_string());
    s.graphics_backend = Some(match ui.get_graphics_backend_index() {
        1 => hmcl_core::launch::GraphicsApi::OpenGl,
        2 => hmcl_core::launch::GraphicsApi::Vulkan,
        _ => hmcl_core::launch::GraphicsApi::Default,
    });
    s.environment_variables = Some(ui.get_environment_variables().to_string());
    s.command_wrapper = Some(ui.get_command_wrapper().to_string());
    s.pre_launch_command = Some(ui.get_pre_launch_command().to_string());
    s.post_exit_command = Some(ui.get_post_exit_command().to_string());
    s.process_priority = Some(match ui.get_process_priority_index() {
        0 => ProcessPriority::Low,
        1 => ProcessPriority::BelowNormal,
        3 => ProcessPriority::AboveNormal,
        4 => ProcessPriority::High,
        _ => ProcessPriority::Normal,
    });
    s.use_custom_natives = Some(ui.get_use_custom_natives());
    s.natives_directory = Some(ui.get_natives_directory().to_string());

    let overridden_flags = [
        (ui.get_java_type_overridden(), PROPERTY_JAVA_TYPE),
        (
            ui.get_custom_java_version_overridden(),
            PROPERTY_CUSTOM_JAVA_VERSION,
        ),
        (
            ui.get_custom_java_path_overridden(),
            PROPERTY_CUSTOM_JAVA_PATH,
        ),
        (ui.get_jvm_options_overridden(), PROPERTY_JVM_OPTIONS),
        (ui.get_no_jvm_options_overridden(), PROPERTY_NO_JVM_OPTIONS),
        (
            ui.get_no_optimizing_jvm_options_overridden(),
            PROPERTY_NO_OPTIMIZING_JVM_OPTIONS,
        ),
        (ui.get_not_check_jvm_overridden(), PROPERTY_NOT_CHECK_JVM),
        (ui.get_auto_memory_overridden(), PROPERTY_AUTO_MEMORY),
        (ui.get_max_memory_overridden(), PROPERTY_MAX_MEMORY),
        (ui.get_min_memory_overridden(), PROPERTY_MIN_MEMORY),
        (ui.get_perm_size_overridden(), PROPERTY_PERM_SIZE),
        (ui.get_window_type_overridden(), PROPERTY_WINDOW_TYPE),
        (ui.get_width_overridden(), PROPERTY_WIDTH),
        (ui.get_height_overridden(), PROPERTY_HEIGHT),
        (ui.get_quick_play_overridden(), PROPERTY_QUICK_PLAY),
        (
            ui.get_quick_play_overridden(),
            PROPERTY_QUICK_PLAY_MULTIPLAYER,
        ),
        (
            ui.get_quick_play_overridden(),
            PROPERTY_QUICK_PLAY_SINGLEPLAYER,
        ),
        (ui.get_quick_play_overridden(), PROPERTY_QUICK_PLAY_REALMS),
        (
            ui.get_launcher_visibility_overridden(),
            PROPERTY_LAUNCHER_VISIBILITY,
        ),
        (
            ui.get_debug_log_overridden(),
            PROPERTY_ENABLE_DEBUG_LOG_OUTPUT,
        ),
        (
            ui.get_running_directory_overridden(),
            PROPERTY_RUNNING_DIRECTORY,
        ),
        (ui.get_game_arguments_overridden(), PROPERTY_GAME_ARGUMENTS),
        (
            ui.get_graphics_backend_overridden(),
            PROPERTY_GRAPHICS_BACKEND,
        ),
        (
            ui.get_environment_variables_overridden(),
            PROPERTY_ENVIRONMENT_VARIABLES,
        ),
        (
            ui.get_command_wrapper_overridden(),
            PROPERTY_COMMAND_WRAPPER,
        ),
        (
            ui.get_pre_launch_command_overridden(),
            PROPERTY_PRE_LAUNCH_COMMAND,
        ),
        (
            ui.get_post_exit_command_overridden(),
            PROPERTY_POST_EXIT_COMMAND,
        ),
        (
            ui.get_process_priority_overridden(),
            PROPERTY_PROCESS_PRIORITY,
        ),
        (
            ui.get_use_custom_natives_overridden(),
            PROPERTY_USE_CUSTOM_NATIVES,
        ),
        (
            ui.get_natives_directory_overridden(),
            PROPERTY_NATIVES_DIRECTORY,
        ),
    ];
    for (checked, property) in overridden_flags {
        if checked {
            s.set_overridden(property);
        }
    }
}

#[derive(Debug)]
struct StageProgress {
    label: String,
    done: usize,
    total: usize,
    state: i32,
    show_count: bool,
}

#[derive(Debug)]
struct FileProgress {
    downloaded: u64,
    total: Option<u64>,
    updated: Instant,
}

#[derive(Debug)]
struct InstallProgress {
    stages: Vec<StageProgress>,
    loader_stage: Option<usize>,
    library_stage: usize,
    asset_stage: usize,
    modpack_archive_stage: Option<usize>,
    modpack_stage: Option<usize>,
    files: BTreeMap<PathBuf, FileProgress>,
    speed_samples: VecDeque<(Instant, u64)>,
    speed_started: Instant,
}

impl InstallProgress {
    fn new(version_id: &str, loader_name: Option<&str>) -> Self {
        let mut stages = vec![StageProgress {
            label: format!("安装 Minecraft {version_id}"),
            done: 0,
            total: 0,
            state: 1,
            show_count: false,
        }];
        let loader_stage = loader_name.map(|name| {
            stages.push(StageProgress {
                label: format!("安装 {name}"),
                done: 0,
                total: 0,
                state: 0,
                show_count: false,
            });
            stages.len() - 1
        });
        let library_stage = stages.len();
        stages.push(StageProgress {
            label: "下载依赖库".to_string(),
            done: 0,
            total: 0,
            state: 0,
            show_count: true,
        });
        let asset_stage = stages.len();
        stages.push(StageProgress {
            label: "下载资源".to_string(),
            done: 0,
            total: 0,
            state: 0,
            show_count: true,
        });

        Self {
            stages,
            loader_stage,
            library_stage,
            asset_stage,
            modpack_archive_stage: None,
            modpack_stage: None,
            files: BTreeMap::new(),
            speed_samples: VecDeque::new(),
            speed_started: Instant::now(),
        }
    }

    fn new_modpack(instance_id: &str, downloads_archive: bool) -> Self {
        let mut progress = Self::new(instance_id, None);
        progress.stages[0].label = format!("创建实例 {instance_id}");
        if downloads_archive {
            progress.stages[0].state = 0;
            progress.stages.insert(
                0,
                StageProgress {
                    label: "下载整合包".to_string(),
                    done: 0,
                    total: 1,
                    state: 1,
                    show_count: false,
                },
            );
            progress.library_stage += 1;
            progress.asset_stage += 1;
            progress.modpack_archive_stage = Some(0);
        }
        progress.modpack_stage = Some(progress.stages.len());
        progress.stages.push(StageProgress {
            label: "下载整合包文件".to_string(),
            done: 0,
            total: 0,
            state: 0,
            show_count: true,
        });
        progress
    }

    fn stage_index(&self, stage: InstallStage) -> Option<usize> {
        match stage {
            InstallStage::Libraries => Some(self.library_stage),
            InstallStage::AssetObjects => Some(self.asset_stage),
            InstallStage::ModpackArchive => self.modpack_archive_stage,
            InstallStage::ModpackFiles => self.modpack_stage,
        }
    }

    fn apply(&mut self, event: ProgressEvent) {
        match event {
            ProgressEvent::LoaderStarted { name } => {
                let index = self.loader_stage.unwrap_or_else(|| {
                    let index = self.library_stage;
                    self.stages.insert(
                        index,
                        StageProgress {
                            label: String::new(),
                            done: 0,
                            total: 0,
                            state: 0,
                            show_count: false,
                        },
                    );
                    self.library_stage += 1;
                    self.asset_stage += 1;
                    if let Some(modpack_stage) = &mut self.modpack_stage {
                        *modpack_stage += 1;
                    }
                    self.loader_stage = Some(index);
                    index
                });
                for row in &mut self.stages[..index] {
                    row.state = 2;
                }
                self.stages[index].label = format!("安装 {name}");
                self.stages[index].state = 1;
            }
            ProgressEvent::LoaderFinished => {
                if let Some(index) = self.loader_stage {
                    self.stages[index].state = 2;
                }
            }
            ProgressEvent::StageStarted { stage, total } => {
                let Some(index) = self.stage_index(stage) else {
                    return;
                };
                for row in &mut self.stages[..index] {
                    row.state = 2;
                    if row.show_count {
                        row.done = row.total;
                    }
                }
                self.stages[index].total = total;
                self.stages[index].state = 1;
            }
            ProgressEvent::TaskDone { stage } => {
                let Some(index) = self.stage_index(stage) else {
                    return;
                };
                let row = &mut self.stages[index];
                row.done = (row.done + 1).min(row.total);
                if row.done == row.total {
                    row.state = 2;
                }
            }
            ProgressEvent::Bytes {
                path,
                chunk_bytes,
                total_bytes,
            } => {
                let now = Instant::now();
                let file = self.files.entry(path).or_insert(FileProgress {
                    downloaded: 0,
                    total: total_bytes,
                    updated: now,
                });
                file.downloaded = file.downloaded.saturating_add(chunk_bytes);
                file.total = total_bytes.or(file.total);
                file.updated = now;
                self.speed_samples.push_back((now, chunk_bytes));

                let mut finished: Vec<_> = self
                    .files
                    .iter()
                    .filter(|(_, file)| file.total.is_some_and(|total| file.downloaded >= total))
                    .map(|(path, file)| (path.clone(), file.updated))
                    .collect();
                finished.sort_by_key(|(_, updated)| std::cmp::Reverse(*updated));
                for (path, _) in finished.into_iter().skip(5) {
                    self.files.remove(&path);
                }
            }
        }
    }

    fn finish(&mut self) {
        for row in &mut self.stages {
            row.state = 2;
            if row.show_count {
                row.done = row.total;
            }
        }
        self.files.clear();
        self.speed_samples.clear();
    }

    fn status_snapshot(&self) -> (Vec<InstallStageRow>, Vec<InstallFileRow>) {
        let stages = self
            .stages
            .iter()
            .map(|row| InstallStageRow {
                label: row.label.clone().into(),
                done: row.done.min(i32::MAX as usize) as i32,
                total: row.total.min(i32::MAX as usize) as i32,
                state: row.state,
                show_count: row.show_count,
            })
            .collect();

        let (mut active, mut finished): (Vec<_>, Vec<_>) = self
            .files
            .iter()
            .partition(|(_, file)| file.total.is_none_or(|total| file.downloaded < total));
        active.sort_by_key(|(_, file)| std::cmp::Reverse(file.updated));
        finished.sort_by_key(|(_, file)| std::cmp::Reverse(file.updated));
        let files = active
            .into_iter()
            .chain(finished)
            .take(5)
            .map(|(path, file)| {
                let total = file.total.unwrap_or(0);
                InstallFileRow {
                    path: path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                    downloaded: if total == 0 {
                        0.0
                    } else {
                        file.downloaded.min(total) as f32
                    },
                    total: total as f32,
                }
            })
            .collect();

        (stages, files)
    }

    fn speed_text(&mut self) -> String {
        let now = Instant::now();
        while self
            .speed_samples
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > Duration::from_secs(1))
        {
            self.speed_samples.pop_front();
        }
        let bytes: u64 = self.speed_samples.iter().map(|(_, bytes)| bytes).sum();
        let elapsed = now
            .duration_since(self.speed_started)
            .min(Duration::from_secs(1))
            .as_secs_f64()
            .max(0.001);
        format_speed(bytes as f64 / elapsed)
    }

    fn snapshot(&mut self) -> (Vec<InstallStageRow>, Vec<InstallFileRow>, String) {
        let (stages, files) = self.status_snapshot();
        (stages, files, self.speed_text())
    }
}

fn format_speed(bytes_per_second: f64) -> String {
    if bytes_per_second >= 1024.0 * 1024.0 {
        format!("{:.1} MiB/s", bytes_per_second / (1024.0 * 1024.0))
    } else if bytes_per_second >= 1024.0 {
        format!("{:.1} KiB/s", bytes_per_second / 1024.0)
    } else {
        format!("{bytes_per_second:.0} B/s")
    }
}

fn valid_download_file_name(file_name: &str) -> bool {
    let file_name = file_name.trim();
    !file_name.is_empty()
        && file_name != "."
        && file_name != ".."
        && Path::new(file_name)
            .file_name()
            .is_some_and(|name| name == Path::new(file_name).as_os_str())
}

fn suggested_instance_name(title: &str) -> String {
    let mut result = String::with_capacity(title.len());
    let mut last_was_separator = false;
    for ch in title.chars() {
        if ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            result.push(ch);
            last_was_separator = false;
        } else if !last_was_separator && !result.is_empty() {
            result.push('-');
            last_was_separator = true;
        }
    }
    let result = result.trim_matches('-');
    if result.is_empty() {
        "modpack".to_string()
    } else {
        result.to_string()
    }
}

fn push_install_progress(ui_weak: &slint::Weak<AppWindow>, progress: &InstallProgress) {
    let (stages, files) = progress.status_snapshot();
    let ui_weak = ui_weak.clone();
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.set_install_stage_lines(slint::ModelRc::new(slint::VecModel::from(stages)));
        ui.set_install_active_files(slint::ModelRc::new(slint::VecModel::from(files)));
    });
}

fn push_install_speed(ui_weak: &slint::Weak<AppWindow>, progress: &mut InstallProgress) {
    let speed = progress.speed_text();
    let ui_weak = ui_weak.clone();
    let _ = ui_weak.upgrade_in_event_loop(move |ui| ui.set_install_speed_text(speed.into()));
}

fn show_modpack_install_progress(
    ui: &AppWindow,
    instance_id: &str,
    downloads_archive: bool,
) -> InstallProgress {
    let mut progress = InstallProgress::new_modpack(instance_id, downloads_archive);
    let (stages, files, speed) = progress.snapshot();
    ui.set_install_title("安装整合包".into());
    ui.set_install_stage_lines(slint::ModelRc::new(slint::VecModel::from(stages)));
    ui.set_install_active_files(slint::ModelRc::new(slint::VecModel::from(files)));
    ui.set_install_speed_text(speed.into());
    ui.set_install_return_to_download_list(true);
    ui.set_show_install_progress(true);
    progress
}

async fn drive_install_progress<F, T, E>(
    ui_weak: slint::Weak<AppWindow>,
    mut progress: InstallProgress,
    mut events: tokio::sync::mpsc::UnboundedReceiver<ProgressEvent>,
    operation: F,
) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
{
    push_install_progress(&ui_weak, &progress);
    push_install_speed(&ui_weak, &mut progress);
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await;
    tokio::pin!(operation);

    let result = loop {
        tokio::select! {
            result = &mut operation => break result,
            Some(event) = events.recv() => {
                progress.apply(event);
                push_install_progress(&ui_weak, &progress);
            },
            _ = ticker.tick() => push_install_speed(&ui_weak, &mut progress),
        }
    };
    while let Ok(event) = events.try_recv() {
        progress.apply(event);
    }
    if result.is_ok() {
        progress.finish();
        push_install_progress(&ui_weak, &progress);
        push_install_speed(&ui_weak, &mut progress);
    }
    result
}

fn push_java_install_progress(
    ui_weak: &slint::Weak<AppWindow>,
    active_files: &Arc<Mutex<BTreeMap<PathBuf, FileProgress>>>,
    progress: hmcl_core::download::mojang_java::MojangJavaProgress,
) {
    let finished = progress.total_files > 0 && progress.completed_files >= progress.total_files;
    let mut active_files = active_files.lock().unwrap();
    let path = PathBuf::from(&progress.path);
    if progress.finished {
        active_files.remove(&path);
    } else {
        active_files.insert(
            path,
            FileProgress {
                downloaded: progress.downloaded,
                total: Some(progress.total_bytes),
                updated: Instant::now(),
            },
        );
    }
    let stages = vec![InstallStageRow {
        label: "下载并安装 Java".into(),
        done: progress.completed_files as i32,
        total: progress.total_files as i32,
        state: if finished { 2 } else { 1 },
        show_count: true,
    }];
    let mut active = active_files.iter().collect::<Vec<_>>();
    active.sort_by_key(|(_, file)| std::cmp::Reverse(file.updated));
    let files: Vec<InstallFileRow> = active
        .into_iter()
        .take(5)
        .map(|(path, file)| InstallFileRow {
            path: path.to_string_lossy().into_owned().into(),
            downloaded: file.downloaded.min(file.total.unwrap_or(0)) as f32,
            total: file.total.unwrap_or(0) as f32,
        })
        .collect();
    drop(active_files);
    let detail = format!(
        "{}/{} 个文件",
        progress.completed_files, progress.total_files
    );
    let ui_weak = ui_weak.clone();
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.set_install_stage_lines(slint::ModelRc::new(slint::VecModel::from(stages)));
        ui.set_install_active_files(slint::ModelRc::new(slint::VecModel::from(files)));
        ui.set_install_speed_text(detail.into());
    });
}

fn loader_kind(index: i32) -> Option<LoaderKind> {
    match index {
        1 => Some(LoaderKind::Forge),
        2 => Some(LoaderKind::NeoForge),
        3 => Some(LoaderKind::OptiFine),
        4 => Some(LoaderKind::Fabric),
        5 => Some(LoaderKind::Quilt),
        _ => None,
    }
}

fn loader_kind_index(kind: LoaderKind) -> i32 {
    LoaderKind::ALL
        .iter()
        .position(|candidate| *candidate == kind)
        .map(|index| index as i32 + 1)
        .unwrap_or(0)
}

/// 从“安装新游戏”配置页真正落盘实例并安装：原版直接装；选择 Forge/NeoForge/
/// OptiFine/Fabric/Quilt 时由 core 先构造 loader patch，再装 client.jar、
/// libraries 和 assets。
///
/// ponytail: 这里直接调 core 的 `game_install::install_game_with_progress`，没有借道
/// `session::install_and_launch(install_only: true)`——那条路要填一堆只有真启动
/// 才用得上的字段（离线用户名、内存上限），纯装的时候全是假值。
async fn install_remote_version(
    ui_weak: slint::Weak<AppWindow>,
    game_dir: PathBuf,
    version_id: String,
    instance_id: String,
    loader_kind_index: i32,
    loader_version: String,
) -> anyhow::Result<()> {
    let client = http_client();
    let provider = configured_download_provider(false);
    let cache = CacheRepository::new(game_dir.join(".hmcl-rs-cache"));
    let repo = GameRepository::new(&game_dir);
    let env = Env {
        platform: Platform::CURRENT,
        os_version: "",
    };

    let loader = loader_kind(loader_kind_index).map(|kind| LoaderSelection {
        kind,
        version: loader_version,
    });
    set_status(&ui_weak, format!("正在准备安装 {instance_id}…"));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let install = game_install::install_game_with_progress(
        &client,
        &provider,
        &cache,
        &repo,
        &game_dir,
        &version_id,
        &instance_id,
        loader.as_ref(),
        env,
        Some(&tx),
    );
    let progress = InstallProgress::new(
        &version_id,
        loader
            .as_ref()
            .map(|selection| selection.kind.display_name()),
    );
    let report = drive_install_progress(ui_weak.clone(), progress, rx, install).await?;

    let lib_failed = report
        .library_results
        .iter()
        .filter(|(_, r)| r.is_err())
        .count();
    let obj_failed = report
        .object_results
        .iter()
        .filter(|(_, r)| r.is_err())
        .count();

    let launcher = load_launcher_settings();
    let global_file = hmcl_core::settings::load::<GlobalGameSettingsFile>(
        &game_settings_path(),
        GAME_SETTINGS_SCHEMA_ID,
    )
    .value;
    let isolation = selected_global_preset(&launcher, &global_file)
        .and_then(|preset| preset.default_isolation_type.as_deref())
        .unwrap_or("MODDED");
    let should_isolate = isolation == "ALWAYS" || (isolation == "MODDED" && loader_kind_index != 0);
    if should_isolate {
        use hmcl_core::settings::instance_game_settings::{
            InstanceGameSettings, PROPERTY_RUNNING_DIRECTORY, SCHEMA_ID,
        };
        let path = hmcl_core::settings::instance_game_settings::instance_settings_path(
            &repo,
            &instance_id,
        );
        let mut loaded = hmcl_core::settings::load::<InstanceGameSettings>(&path, SCHEMA_ID);
        if loaded.can_save {
            loaded.value.running_directory = Some(String::new());
            loaded.value.set_overridden(PROPERTY_RUNNING_DIRECTORY);
            hmcl_core::settings::save(&path, SCHEMA_ID, &loaded.value)?;
        }
    }

    if lib_failed == 0 && obj_failed == 0 {
        set_status(
            &ui_weak,
            format!(
                "{instance_id} 安装完成（依赖库 {} 个, 资源 {} 个）",
                report.library_results.len(),
                report.object_results.len()
            ),
        );
    } else {
        set_status(&ui_weak, format!("{instance_id} 装完了但有失败项: 依赖库 {lib_failed} 个失败, 资源 {obj_failed} 个失败"));
    }
    Ok(())
}

async fn install_instance_loader(
    ui_weak: slint::Weak<AppWindow>,
    game_dir: PathBuf,
    instance_id: String,
    selection: LoaderSelection,
) -> anyhow::Result<String> {
    let client = http_client();
    let provider = configured_download_provider(false);
    let cache = CacheRepository::new(game_dir.join(".hmcl-rs-cache"));
    let repo = GameRepository::new(&game_dir);
    let env = Env {
        platform: Platform::CURRENT,
        os_version: "",
    };
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let install = game_install::install_loader_with_progress(
        &client,
        &provider,
        &cache,
        &repo,
        &game_dir,
        &instance_id,
        &selection,
        env,
        Some(&tx),
    );
    let progress = InstallProgress::new(&instance_id, Some(selection.kind.display_name()));
    let report = drive_install_progress(ui_weak.clone(), progress, rx, install).await?;

    let failed = report
        .library_results
        .iter()
        .chain(&report.object_results)
        .filter(|(_, result)| result.is_err())
        .count();
    if failed == 0 {
        set_status(
            &ui_weak,
            format!(
                "{} {} 安装完成",
                selection.kind.display_name(),
                selection.version
            ),
        );
    } else {
        set_status(
            &ui_weak,
            format!(
                "{} 安装完成，但有 {failed} 个文件下载失败",
                selection.kind.display_name()
            ),
        );
    }
    sync_instance_loader_name(&game_dir, &instance_id, Some(selection.kind))
        .map_err(anyhow::Error::msg)
}

async fn launch_instance(
    ui_weak: slint::Weak<AppWindow>,
    game_dir: PathBuf,
    instance_id: String,
    account: KnownAccount,
    script_output: Option<PathBuf>,
    quick_play_world: Option<String>,
    // "取消启动"按下时会被 notify。安装阶段靠丢弃 future 取消，游戏已经跑起来
    // 之后则是真的把游戏进程杀掉——以前这里是 `task.abort()`，abort 只是让异步
    // 任务消失，tokio 的 Child 默认不 kill-on-drop，游戏会变成后台孤儿进程继续跑。
    cancel: std::sync::Arc<tokio::sync::Notify>,
) {
    let client = http_client();
    let provider = configured_download_provider(false);
    let auth = match account_auth(&client, &provider, &account).await {
        Ok(auth) => auth,
        Err(error) => {
            set_status(&ui_weak, error);
            let _ = ui_weak.upgrade_in_event_loop(|ui| {
                ui.set_show_launch_progress(false);
            });
            return;
        }
    };
    let cache = CacheRepository::new(game_dir.join(".hmcl-rs-cache"));
    let repo = GameRepository::new(&game_dir);
    let env = Env {
        platform: Platform::CURRENT,
        os_version: "",
    };

    let all = repo.load_all_versions();
    let Some(raw) = all.get(&instance_id) else {
        set_status(&ui_weak, format!("找不到实例 {instance_id}"));
        return;
    };
    let version = match raw.resolve(&all) {
        Ok(v) => v,
        Err(e) => {
            set_status(&ui_weak, format!("实例解析失败: {e}"));
            return;
        }
    };

    let launcher_settings = load_launcher_settings();
    let global_file = hmcl_core::settings::load::<GlobalGameSettingsFile>(
        &game_settings_path(),
        GAME_SETTINGS_SCHEMA_ID,
    )
    .value;
    let global_preset = selected_global_preset(&launcher_settings, &global_file);
    let default_max_memory = global_preset
        .and_then(|preset| preset.max_memory)
        .unwrap_or(2048);
    let default_auto_memory = global_preset
        .and_then(|preset| preset.auto_memory)
        .unwrap_or(true);
    let default_window_width = global_preset
        .and_then(|preset| preset.width)
        .unwrap_or(1280.0)
        .round() as i32;
    let default_window_height = global_preset
        .and_then(|preset| preset.height)
        .unwrap_or(720.0)
        .round() as i32;
    let default_debug_log_output = global_preset
        .and_then(|preset| preset.enable_debug_log_output)
        .unwrap_or(false);

    use hmcl_core::settings::instance_game_settings::{
        JavaSelectionType, LauncherVisibility, PROPERTY_JAVA_TYPE, PROPERTY_LAUNCHER_VISIBILITY,
    };
    let instance_settings = hmcl_core::settings::instance_game_settings::load(&repo, &version.id);
    let recommended_java_major = version.java_version.as_ref().map(|java| java.major_version);
    let visibility = if instance_settings.is_overridden(PROPERTY_LAUNCHER_VISIBILITY) {
        instance_settings.effective_launcher_visibility()
    } else {
        match global_preset.and_then(|preset| preset.launcher_visibility.as_deref()) {
            Some("CLOSE") => LauncherVisibility::Close,
            Some("HIDE") => LauncherVisibility::Hide,
            Some("HIDE_AND_REOPEN") => LauncherVisibility::HideAndReopen,
            _ => LauncherVisibility::Keep,
        }
    };
    let java_override = if instance_settings.is_overridden(PROPERTY_JAVA_TYPE) {
        match instance_settings
            .java_type
            .unwrap_or(JavaSelectionType::Auto)
        {
            JavaSelectionType::Custom => instance_settings
                .custom_java_path
                .as_deref()
                .filter(|path| !path.is_empty())
                .map(PathBuf::from),
            JavaSelectionType::Version => instance_settings
                .custom_java_version
                .as_deref()
                .and_then(managed_java_binary),
            JavaSelectionType::Auto | JavaSelectionType::Detected => {
                find_preferred_java(recommended_java_major)
                    .ok()
                    .map(|java| java.binary)
            }
        }
    } else {
        match global_preset.and_then(|preset| preset.java_type.as_deref()) {
            Some("CUSTOM") => global_preset
                .and_then(|preset| preset.custom_java_path.as_deref())
                .filter(|path| !path.is_empty())
                .map(PathBuf::from),
            Some("VERSION") => global_preset
                .and_then(|preset| preset.custom_java_version.as_deref())
                .and_then(managed_java_binary),
            _ => None,
        }
        .or_else(|| {
            find_preferred_java(recommended_java_major)
                .ok()
                .map(|java| java.binary)
        })
    };

    let default_quick_play_option = global_preset.and_then(|preset| {
        use hmcl_core::launch::QuickPlayOption;
        match preset.quick_play.as_deref() {
            Some("MULTIPLAYER") => preset
                .quick_play_multiplayer
                .clone()
                .filter(|value| !value.is_empty())
                .map(|server_ip| QuickPlayOption::MultiPlayer { server_ip }),
            Some("SINGLEPLAYER") => preset
                .quick_play_singleplayer
                .clone()
                .filter(|value| !value.is_empty())
                .map(|world_folder_name| QuickPlayOption::SinglePlayer { world_folder_name }),
            Some("REALMS") => preset
                .quick_play_realms
                .clone()
                .filter(|value| !value.is_empty())
                .map(|realm_id| QuickPlayOption::Realm { realm_id }),
            _ => None,
        }
    });
    let req = LaunchRequest {
        client: &client,
        provider: &provider,
        cache: &cache,
        repo: &repo,
        dir: &game_dir,
        env,
        version,
        auth,
        default_max_memory,
        default_auto_memory,
        default_min_memory: global_preset.and_then(|preset| preset.min_memory),
        default_metaspace: global_preset.and_then(|preset| preset.perm_size),
        default_window_width,
        default_window_height,
        default_fullscreen: global_preset.and_then(|preset| preset.window_type.as_deref())
            == Some("FULLSCREEN"),
        default_debug_log_output,
        default_no_jvm_options: global_preset
            .and_then(|preset| preset.no_jvm_options)
            .unwrap_or(false),
        default_no_optimizing_jvm_options: global_preset
            .and_then(|preset| preset.no_optimizing_jvm_options)
            .unwrap_or(false),
        default_jvm_options: global_preset.and_then(|preset| preset.jvm_options.clone()),
        default_game_arguments: global_preset.and_then(|preset| preset.game_arguments.clone()),
        default_quick_play_option,
        quick_play_override: quick_play_world.map(|world_folder_name| {
            hmcl_core::launch::QuickPlayOption::SinglePlayer { world_folder_name }
        }),
        default_wrapper: global_preset.and_then(|preset| preset.command_wrapper.clone()),
        default_process_priority: match global_preset
            .and_then(|preset| preset.process_priority.as_deref())
        {
            Some("LOW") => hmcl_core::launch::ProcessPriority::Low,
            Some("BELOW_NORMAL") => hmcl_core::launch::ProcessPriority::BelowNormal,
            Some("ABOVE_NORMAL") => hmcl_core::launch::ProcessPriority::AboveNormal,
            Some("HIGH") => hmcl_core::launch::ProcessPriority::High,
            _ => hmcl_core::launch::ProcessPriority::Normal,
        },
        default_graphics_backend: match global_preset
            .and_then(|preset| preset.graphics_backend.as_deref())
        {
            Some("OPENGL") => hmcl_core::launch::GraphicsApi::OpenGl,
            Some("VULKAN") => hmcl_core::launch::GraphicsApi::Vulkan,
            _ => hmcl_core::launch::GraphicsApi::Default,
        },
        default_environment_variables: global_preset
            .and_then(|preset| preset.environment_variables.clone()),
        default_pre_launch_command: global_preset
            .and_then(|preset| preset.pre_launch_command.clone()),
        default_post_exit_command: global_preset
            .and_then(|preset| preset.post_exit_command.clone()),
        default_use_custom_natives: global_preset
            .and_then(|preset| preset.use_custom_natives)
            .unwrap_or(false),
        default_natives_directory: global_preset
            .and_then(|preset| preset.natives_directory.clone()),
        install_only: false,
        java_override,
    };

    let event_ui_weak = ui_weak.clone();
    let on_event = move |event| {
        let (stage, line) = match event {
            LaunchEvent::InstallSummary(report) => {
                let lib_failures = report
                    .library_results
                    .iter()
                    .filter(|(_, r)| r.is_err())
                    .count();
                let obj_failures = report
                    .object_results
                    .iter()
                    .filter(|(_, r)| r.is_err())
                    .count();
                (
                    1,
                    format!(
                        "安装完成: libraries {}/{} 成功, assets {}/{} 成功",
                        report.library_results.len() - lib_failures,
                        report.library_results.len(),
                        report.object_results.len() - obj_failures,
                        report.object_results.len()
                    ),
                )
            }
            LaunchEvent::JavaDetected(java) => (
                2,
                format!(
                    "Java: {} ({})",
                    java.binary.display(),
                    java.parsed_version()
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "?".to_string())
                ),
            ),
            LaunchEvent::CommandLine(_) => (3, "已生成启动命令行".to_string()),
            LaunchEvent::Warning(message) => (usize::MAX, message),
        };
        if stage != usize::MAX {
            push_launch_progress(&event_ui_weak, stage, line.clone());
        }
        set_status(&event_ui_weak, line);
    };
    let launch = async {
        match script_output.as_deref() {
            Some(output) => session::generate_launch_script(req, output, on_event)
                .await
                .map(|_| None),
            None => session::install_and_launch(req, on_event).await,
        }
    };
    let result = tokio::select! {
        result = launch => result,
        _ = cancel.notified() => {
            let close_ui = ui_weak.clone();
            let _ = close_ui.upgrade_in_event_loop(|ui| ui.set_show_launch_progress(false));
            set_status(&ui_weak, "已取消启动".to_string());
            return;
        }
    };

    if let Some(output) = script_output {
        match result {
            Ok(_) => set_status(&ui_weak, format!("启动脚本已保存到 {}", output.display())),
            Err(e) => set_status(&ui_weak, format!("生成启动脚本失败: {e}")),
        }
        return;
    }

    match result {
        Ok(Some(launched)) => {
            push_launch_progress(&ui_weak, 4, "游戏进程已启动".to_string());
            tokio::time::sleep(Duration::from_millis(120)).await;
            let close_ui = ui_weak.clone();
            let _ = close_ui.upgrade_in_event_loop(|ui| ui.set_show_launch_progress(false));
            set_status(&ui_weak, "运行中".to_string());
            apply_launcher_visibility(&ui_weak, visibility, true);

            let mut process = launched.process;
            let stdout = process.child.stdout.take().unwrap();
            let stderr = process.child.stderr.take().unwrap();
            let stdout_ui_weak = ui_weak.clone();
            let stderr_ui_weak = ui_weak.clone();
            let recent_logs = Arc::new(Mutex::new(VecDeque::new()));
            let stdout_logs = recent_logs.clone();
            let stderr_logs = recent_logs.clone();
            let stdout_task = tokio::spawn(launch::pump_lines(stdout, move |line| {
                retain_recent_game_log(&stdout_logs, &line);
                set_status(&stdout_ui_weak, line)
            }));
            let stderr_task = tokio::spawn(launch::pump_lines(stderr, move |line| {
                retain_recent_game_log(&stderr_logs, &line);
                set_status(&stderr_ui_weak, line)
            }));

            let mut cancelled = false;
            let status = tokio::select! {
                status = process.wait() => status,
                _ = cancel.notified() => {
                    cancelled = true;
                    let _ = process.stop();
                    process.wait().await
                }
            };
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            apply_launcher_visibility(&ui_weak, visibility, false);
            let close_ui = ui_weak.clone();
            let _ = close_ui.upgrade_in_event_loop(|ui| ui.set_show_launch_progress(false));
            let crashed = !cancelled && status.as_ref().map_or(true, |status| !status.success());
            let exit_label = match &status {
                Ok(status) => status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| status.to_string()),
                Err(error) => format!("无法读取退出状态: {error}"),
            };
            set_status(
                &ui_weak,
                if cancelled {
                    "已取消启动，游戏进程已结束".to_string()
                } else {
                    format!("游戏进程退出: {status:?}")
                },
            );
            if crashed {
                let run_directory = repo.run_directory(&instance_id);
                let logs_directory = run_directory.join("logs");
                let log = recent_game_log_text(&recent_logs, &logs_directory.join("latest.log"));
                let log_lines = log
                    .lines()
                    .map(|line| CrashLogLine {
                        text: line.into(),
                        level: crash_log_level(line),
                    })
                    .collect::<Vec<_>>();
                let log_folder = if logs_directory.is_dir() {
                    logs_directory
                } else {
                    run_directory
                };
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_crash_exit_code(exit_label.into());
                    ui.set_crash_log(log.into());
                    ui.set_crash_log_lines(slint::ModelRc::new(slint::VecModel::from(log_lines)));
                    ui.set_crash_log_folder(log_folder.to_string_lossy().into_owned().into());
                    ui.set_show_crash_dialog(true);
                });
            }

            if let Some(post_exit) = launched.post_exit_command {
                if let Err(e) = session::run_user_command(&post_exit).await {
                    set_status(&ui_weak, format!("postExitCommand 执行失败: {e}"));
                }
            }
        }
        Ok(None) => {
            let close_ui = ui_weak.clone();
            let _ = close_ui.upgrade_in_event_loop(|ui| ui.set_show_launch_progress(false));
            set_status(&ui_weak, "已装好(install_only)".to_string());
        }
        Err(e) => {
            // 启动失败也要把窗口还原, 不然选了 Close/Hide 的实例装失败之后启动器
            // 就再也叫不出来了。
            apply_launcher_visibility(&ui_weak, visibility, false);
            let close_ui = ui_weak.clone();
            let _ = close_ui.upgrade_in_event_loop(|ui| ui.set_show_launch_progress(false));
            set_status(&ui_weak, format!("启动失败: {e}"));
        }
    }
}

/// 对应 Java `LauncherVisibility`：`before_launch=true` 是进程刚起来之前调一次，
/// `before_launch=false` 是进程退出后（或者启动失败）调一次。两处简化：
/// - `Close` 目前跟 `Hide` 表现一样都是"藏起来"——真启动器选 Close 是连主进程
///   一起退出，这里没有那么做，因为 GUI 唯一的事件循环就是这个窗口自己的，
///   退出会直接干掉整个进程，把还没跑完的游戏子进程一起带走，代价太大，不值得
///   为了这一个冷门选项冒这个险；先做成"隐藏但保留进程"，等真的有人抱怨再改。
/// - `Hide` 跟 `HideAndReopen` 现在表现完全一样（退出后都会重新显示）——没有
///   核实到 Java 版 `Hide`"只在失败时重新显示、正常退出后是不是保持隐藏"这个
///   细节的准确语义，两者收敛成同一种更直观的行为（游戏退出/失败都会显示）
///   比猜一个可能猜错的区分更安全。
fn apply_launcher_visibility(
    ui_weak: &slint::Weak<AppWindow>,
    visibility: hmcl_core::settings::instance_game_settings::LauncherVisibility,
    before_launch: bool,
) {
    use hmcl_core::settings::instance_game_settings::LauncherVisibility;
    let ui_weak = ui_weak.clone();
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        let window = ui.window();
        match (visibility, before_launch) {
            (LauncherVisibility::Keep, _) => {}
            (
                LauncherVisibility::Close
                | LauncherVisibility::Hide
                | LauncherVisibility::HideAndReopen,
                true,
            ) => {
                let _ = window.hide();
            }
            (
                LauncherVisibility::Close
                | LauncherVisibility::Hide
                | LauncherVisibility::HideAndReopen,
                false,
            ) => {
                let _ = window.show();
            }
        }
    });
}

/// 双击运行时不该弹黑框，从 cmd/PowerShell 里运行时又该能看到输出——这两件事在
/// Windows 上是靠 `windows_subsystem = "windows"` **加上**运行时挂到父进程的控制台
/// 来同时满足的：
///
/// - 双击启动：父进程（explorer）没有控制台，`AttachConsole` 失败，什么都不做，
///   于是全程无窗口。
/// - 从 cmd 启动：挂上 cmd 那个控制台，`println!`/`eprintln!`/panic 回溯就直接打在
///   用户正在看的窗口里。
///
/// 只在标准句柄确实是空的时候才去接管，否则会把用户自己写的 `> log.txt` 重定向
/// 覆盖掉。
///
/// ponytail: 手写这几个 FFI 声明，没有为 4 个函数引入 `windows-sys`——它有 4 个
/// 版本在依赖树里，挑哪个都要多编一份。
#[cfg(windows)]
fn attach_parent_console() {
    use std::ffi::c_void;

    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;

    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetStdHandle(which: u32, handle: *mut c_void) -> i32;
        fn CreateFileW(
            file_name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: *mut c_void,
        ) -> *mut c_void;
    }

    let invalid_handle = usize::MAX as *mut c_void;
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        for (which, device) in [
            (STD_OUTPUT_HANDLE, "CONOUT$\0"),
            (STD_ERROR_HANDLE, "CONOUT$\0"),
            (STD_INPUT_HANDLE, "CONIN$\0"),
        ] {
            let existing = GetStdHandle(which);
            if !existing.is_null() && existing != invalid_handle {
                continue;
            }
            let name: Vec<u16> = device.encode_utf16().collect();
            let handle = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if handle != invalid_handle {
                SetStdHandle(which, handle);
            }
        }
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

fn main() -> anyhow::Result<()> {
    attach_parent_console();
    let ui = AppWindow::new()?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(16 * 1024 * 1024)
        .build()?;
    let handle = rt.handle().clone();
    let install_task: Rc<RefCell<Option<tokio::task::JoinHandle<()>>>> =
        Rc::new(RefCell::new(None));
    let launch_task: Rc<RefCell<Option<tokio::task::JoinHandle<()>>>> = Rc::new(RefCell::new(None));
    let microsoft_login_task: Rc<RefCell<Option<tokio::task::JoinHandle<()>>>> =
        Rc::new(RefCell::new(None));
    let authlib_login_task: Rc<RefCell<Option<tokio::task::JoinHandle<()>>>> =
        Rc::new(RefCell::new(None));
    let pending_authlib_server = Arc::new(Mutex::new(None));
    let pending_authlib_login: Arc<Mutex<Option<PendingAuthlibLogin>>> = Arc::new(Mutex::new(None));
    let launch_cancel: Rc<RefCell<Option<std::sync::Arc<tokio::sync::Notify>>>> =
        Rc::new(RefCell::new(None));

    initialize_game_directories();
    let initial_game_dir = resolve_game_dir();

    refresh_game_directories(&ui);
    refresh_accounts(&ui);
    refresh_authlib_injector_servers(&ui);
    restore_selected_account(&ui);
    refresh_instances(&ui, &initial_game_dir, "");
    restore_selected_instance(&ui);
    let launcher_settings = load_launcher_settings();
    populate_launcher_settings_ui(&ui, &launcher_settings);
    let global_settings = hmcl_core::settings::load::<GlobalGameSettingsFile>(
        &game_settings_path(),
        GAME_SETTINGS_SCHEMA_ID,
    )
    .value;
    populate_global_settings_ui(&ui, &launcher_settings, &global_settings);

    {
        let ui_weak = ui.as_weak();
        ui.on_save_launcher_settings(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let path = launcher_settings_path();
            let mut loaded = hmcl_core::settings::load::<LauncherSettings>(
                &path,
                hmcl_core::settings::launcher_settings::SCHEMA_ID,
            );
            if !loaded.can_save {
                ui.set_status_text("启动器设置文件 schema 无法识别，已拒绝覆盖".into());
                return;
            }
            apply_ui_to_launcher_settings(&ui, &mut loaded.value);
            if let Err(e) = hmcl_core::settings::save(
                &path,
                hmcl_core::settings::launcher_settings::SCHEMA_ID,
                &loaded.value,
            ) {
                ui.set_status_text(format!("保存启动器设置失败: {e}").into());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_save_global_settings(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let path = game_settings_path();
            let mut loaded =
                hmcl_core::settings::load::<GlobalGameSettingsFile>(&path, GAME_SETTINGS_SCHEMA_ID);
            if !loaded.can_save {
                ui.set_status_text("全局游戏设置文件 schema 无法识别，已拒绝覆盖".into());
                return;
            }
            let launcher = load_launcher_settings();
            let selected_id = launcher.default_game_settings_preset.as_deref();
            let preset_index = selected_id
                .and_then(|id| {
                    loaded
                        .value
                        .presets
                        .iter()
                        .position(|preset| preset.id == id)
                })
                .or_else(|| (!loaded.value.presets.is_empty()).then_some(0));
            let index = match preset_index {
                Some(index) => index,
                None => {
                    loaded.value.presets.push(GlobalGameSettingsPreset {
                        id: DEFAULT_PRESET_ID.to_string(),
                        ..Default::default()
                    });
                    0
                }
            };
            apply_ui_to_global_preset(&ui, &mut loaded.value.presets[index]);
            if let Err(e) = hmcl_core::settings::save(&path, GAME_SETTINGS_SCHEMA_ID, &loaded.value)
            {
                ui.set_status_text(format!("保存全局游戏设置失败: {e}").into());
                return;
            }

            if launcher.default_game_settings_preset.as_deref()
                != Some(loaded.value.presets[index].id.as_str())
            {
                let launcher_path = launcher_settings_path();
                let mut launcher_loaded = hmcl_core::settings::load::<LauncherSettings>(
                    &launcher_path,
                    hmcl_core::settings::launcher_settings::SCHEMA_ID,
                );
                if launcher_loaded.can_save {
                    launcher_loaded.value.default_game_settings_preset =
                        Some(loaded.value.presets[index].id.clone());
                    if let Err(e) = hmcl_core::settings::save(
                        &launcher_path,
                        hmcl_core::settings::launcher_settings::SCHEMA_ID,
                        &launcher_loaded.value,
                    ) {
                        ui.set_status_text(format!("保存默认游戏设置预设失败: {e}").into());
                    }
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_refresh_java(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            refresh_java_ui(&ui);
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_open_java_folder(move |path| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if let Err(e) = open_java_folder(Path::new(path.as_str())) {
                ui.set_status_text(format!("打开 Java 目录失败: {e}").into());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let install_task = install_task.clone();
        ui.on_download_java(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if ui.get_java_download_loading() {
                return;
            }
            use hmcl_core::download::mojang_java::MojangJavaComponent;
            let component = match index {
                0 => MojangJavaComponent::JreLegacy,
                1 => MojangJavaComponent::RuntimeAlpha,
                2 => MojangJavaComponent::RuntimeBeta,
                3 => MojangJavaComponent::RuntimeDelta,
                _ => MojangJavaComponent::RuntimeEpsilon,
            };
            let major = match index {
                0 => 8,
                1 => 16,
                2 => 17,
                3 => 21,
                _ => 25,
            };
            ui.set_java_download_loading(true);
            ui.set_install_title(format!("安装 Java {major}").into());
            ui.set_install_stage_lines(slint::ModelRc::new(slint::VecModel::from(vec![
                InstallStageRow {
                    label: "正在获取下载清单".into(),
                    done: 0,
                    total: 0,
                    state: 1,
                    show_count: false,
                },
            ])));
            ui.set_install_active_files(slint::ModelRc::new(slint::VecModel::from(Vec::<
                InstallFileRow,
            >::new(
            ))));
            ui.set_install_speed_text("正在准备…".into());
            ui.set_install_return_to_download_list(true);
            ui.set_show_install_progress(true);

            let ui_weak = ui_weak.clone();
            let task = handle.spawn(async move {
                let client = http_client();
                let provider = configured_download_provider(false);
                let install_root = launcher_data_dir().join("java");
                let progress_ui = ui_weak.clone();
                let active_files = Arc::new(Mutex::new(BTreeMap::new()));
                let progress_files = active_files.clone();
                let result = hmcl_core::download::mojang_java::install_mojang_java_with_progress(
                    &client,
                    &provider,
                    &install_root,
                    component,
                    move |progress| {
                        push_java_install_progress(&progress_ui, &progress_files, progress)
                    },
                )
                .await;
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_java_download_loading(false);
                    match result {
                        Ok(_) => {
                            refresh_java_ui(&ui);
                            ui.set_status_text("Java 下载并安装完成".into());
                            ui.set_install_succeeded(true);
                        }
                        Err(e) => {
                            ui.set_show_install_progress(false);
                            ui.set_status_text(format!("Java 下载失败: {e}").into());
                        }
                    }
                });
            });
            if let Some(previous) = install_task.borrow_mut().replace(task) {
                previous.abort();
            }
        });
    }
    ui.invoke_refresh_java();

    ui.on_derive_offline_uuid(|username| {
        hmcl_core::auth::offline_player_uuid(username.trim())
            .to_string()
            .into()
    });
    ui.on_valid_uuid(|value| uuid::Uuid::parse_str(value.trim()).is_ok());
    ui.on_suggest_instance_name(|title| suggested_instance_name(&title).into());
    {
        ui.on_instance_name_exists(move |name| {
            let game_dir = resolve_game_dir();
            GameRepository::new(&game_dir)
                .version_json_path(name.trim())
                .is_file()
        });
    }

    ui.on_select_account(move |index| {
        if index < 0 {
            return;
        }
        let accounts = hmcl_core::settings::load::<AccountsFile>(
            &accounts_file_path(),
            hmcl_core::settings::accounts::SCHEMA_ID,
        )
        .value
        .known_accounts();
        if let Some(account) = accounts.get(index as usize) {
            set_selected_account(account.account_id());
        }
    });
    {
        let ui_weak = ui.as_weak();
        ui.on_copy_account_uuid(move |index| {
            if index < 0 {
                return;
            }
            let Some(ui) = ui_weak.upgrade() else { return };
            let accounts = hmcl_core::settings::load::<AccountsFile>(
                &accounts_file_path(),
                hmcl_core::settings::accounts::SCHEMA_ID,
            )
            .value
            .known_accounts();
            let Some(account) = accounts.get(index as usize) else {
                return;
            };
            let uuid = match account {
                KnownAccount::Offline(account) => account.resolved_profile_id().to_string(),
                KnownAccount::Microsoft(account) => account.profile_id.clone(),
                KnownAccount::AuthlibInjector(account) => account.profile_id.clone(),
            };
            #[cfg(windows)]
            match clipboard_win::set_clipboard_string(&uuid) {
                Ok(()) => ui.set_status_text("UUID 已复制".into()),
                Err(error) => ui.set_status_text(format!("复制 UUID 失败: {error}").into()),
            }
            #[cfg(not(windows))]
            ui.set_status_text("当前平台暂不支持复制 UUID".into());
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_select_instance(move |index| {
            if index < 0 {
                return;
            }
            let Some(ui) = ui_weak.upgrade() else { return };
            if let Some(row) = ui.get_instances().row_data(index as usize) {
                set_selected_instance(row.id.as_str());
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_window_drag(move |dx, dy| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let window = ui.window();
            let scale = window.scale_factor();
            let logical = window.position().to_logical(scale);
            let moved = slint::LogicalPosition::new(logical.x + dx, logical.y + dy);
            window.set_position(moved.to_physical(scale));
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_window_minimize(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            ui.window().set_minimized(true);
        });
    }
    ui.on_window_close(move || {
        let _ = slint::quit_event_loop();
    });

    {
        let ui_weak = ui.as_weak();
        ui.on_stub_clicked(move |message| {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_status_text(message);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_open_url(move |url| {
            if let Err(error) = open_url(url.as_str()) {
                set_status(&ui_weak, format!("打开网页失败: {error}"));
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_open_game_version_wiki(move |version| {
            if let Err(error) = open_url(&minecraft_wiki_url(version.as_str())) {
                set_status(&ui_weak, format!("打开版本百科失败: {error}"));
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_open_crash_log_folder(move |path| {
            if let Err(error) = reveal_directory(Path::new(path.as_str())) {
                set_status(&ui_weak, format!("打开日志文件夹失败: {error}"));
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_copy_text(move |text| {
            #[cfg(windows)]
            match clipboard_win::set_clipboard_string(text.as_str()) {
                Ok(()) => set_status(&ui_weak, "已复制".to_string()),
                Err(error) => set_status(&ui_weak, format!("复制失败: {error}")),
            }
            #[cfg(not(windows))]
            set_status(&ui_weak, "当前平台暂不支持复制".to_string());
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        ui.on_add_game_directory(move || {
            let ui_weak = ui_weak.clone();
            handle.spawn(async move {
                let Some(folder) = rfd::AsyncFileDialog::new()
                    .set_title("选择游戏文件夹")
                    .pick_folder()
                    .await
                else {
                    return;
                };
                let selected_path = folder.path().to_path_buf();
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    let file_path = game_directories_file_path();
                    let mut loaded = hmcl_core::settings::load::<GameDirectoriesFile>(
                        &file_path,
                        hmcl_core::settings::game_directories::SCHEMA_ID,
                    );
                    if !loaded.can_save {
                        ui.set_status_text("游戏文件夹配置版本不受支持，无法添加目录".into());
                        return;
                    }
                    if loaded.value.directories.is_empty() {
                        loaded.value.directories.push(default_game_directory());
                    }
                    if let Some(existing) = loaded.value.directories.iter().find(|directory| {
                        same_game_directory_path(Path::new(&directory.path), &selected_path)
                    }) {
                        let id = existing.id.clone();
                        if let Err(error) = set_selected_game_directory(&id) {
                            ui.set_status_text(format!("切换游戏文件夹失败: {error}").into());
                            return;
                        }
                        reload_selected_game_directory(&ui);
                        ui.set_status_text("该游戏文件夹已经在列表中".into());
                        return;
                    }
                    let directory = GameDirectory::new(
                        Some(suggested_game_directory_name(&selected_path)),
                        selected_path.to_string_lossy().into_owned(),
                    );
                    let id = directory.id.clone();
                    loaded.value.directories.push(directory);
                    if let Err(error) = hmcl_core::settings::save(
                        &file_path,
                        hmcl_core::settings::game_directories::SCHEMA_ID,
                        &loaded.value,
                    ) {
                        ui.set_status_text(format!("保存游戏文件夹失败: {error}").into());
                        return;
                    }
                    if let Err(error) = set_selected_game_directory(&id) {
                        ui.set_status_text(format!("切换游戏文件夹失败: {error}").into());
                        return;
                    }
                    reload_selected_game_directory(&ui);
                    ui.set_status_text("已添加并切换游戏文件夹".into());
                });
            });
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_select_game_directory(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(directory) = ui.get_game_directories().row_data(index as usize) else {
                return;
            };
            if let Err(error) = set_selected_game_directory(directory.id.as_str()) {
                ui.set_status_text(format!("切换游戏文件夹失败: {error}").into());
                return;
            }
            reload_selected_game_directory(&ui);
            ui.set_status_text(format!("当前游戏文件夹: {}", directory.name).into());
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_rename_game_directory(move |index, name| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(row) = ui.get_game_directories().row_data(index as usize) else {
                return;
            };
            let name = name.trim();
            if name.is_empty() {
                return;
            }
            let path = game_directories_file_path();
            let mut loaded = hmcl_core::settings::load::<GameDirectoriesFile>(
                &path,
                hmcl_core::settings::game_directories::SCHEMA_ID,
            );
            if !loaded.can_save {
                ui.set_status_text("游戏文件夹配置版本不受支持，无法重命名".into());
                return;
            }
            let Some(directory) = loaded
                .value
                .directories
                .iter_mut()
                .find(|directory| directory.id == row.id.as_str())
            else {
                return;
            };
            directory.name = Some(LocalizedText::Plain(name.to_string()));
            match hmcl_core::settings::save(
                &path,
                hmcl_core::settings::game_directories::SCHEMA_ID,
                &loaded.value,
            ) {
                Ok(()) => {
                    refresh_game_directories(&ui);
                    ui.set_status_text("游戏文件夹已重命名".into());
                }
                Err(error) => ui.set_status_text(format!("重命名游戏文件夹失败: {error}").into()),
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_remove_game_directory(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(row) = ui.get_game_directories().row_data(index as usize) else {
                return;
            };
            let directories_path = game_directories_file_path();
            let mut loaded = hmcl_core::settings::load::<GameDirectoriesFile>(
                &directories_path,
                hmcl_core::settings::game_directories::SCHEMA_ID,
            );
            if !loaded.can_save {
                ui.set_status_text("游戏文件夹配置版本不受支持，无法移除".into());
                return;
            }
            if loaded.value.directories.len() <= 1 {
                ui.set_status_text("至少需要保留一个游戏文件夹".into());
                return;
            }
            loaded
                .value
                .directories
                .retain(|directory| directory.id != row.id.as_str());
            if let Err(error) = hmcl_core::settings::save(
                &directories_path,
                hmcl_core::settings::game_directories::SCHEMA_ID,
                &loaded.value,
            ) {
                ui.set_status_text(format!("移除游戏文件夹失败: {error}").into());
                return;
            }

            let settings_path = launcher_settings_path();
            let mut settings = hmcl_core::settings::load::<LauncherSettings>(
                &settings_path,
                hmcl_core::settings::launcher_settings::SCHEMA_ID,
            );
            settings.value.selected_instance.remove(row.id.as_str());
            if settings.value.selected_game_directory.as_deref() == Some(row.id.as_str()) {
                settings.value.selected_game_directory = loaded
                    .value
                    .directories
                    .first()
                    .map(|directory| directory.id.clone());
            }
            if settings.can_save {
                let _ = hmcl_core::settings::save(
                    &settings_path,
                    hmcl_core::settings::launcher_settings::SCHEMA_ID,
                    &settings.value,
                );
            }
            reload_selected_game_directory(&ui);
            ui.set_status_text("已从列表移除游戏文件夹，磁盘文件未删除".into());
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_filter_changed(move |text| {
            let game_dir = resolve_game_dir();
            if let Some(ui) = ui_weak.upgrade() {
                filter_instances(&ui, &game_dir, &text);
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_add_account(move |username, custom_uuid| {
            let username = username.trim().to_string();
            if username.is_empty() {
                return;
            }
            let Some(ui) = ui_weak.upgrade() else { return };

            let mut entry = OfflineAccountEntry::new(&username);
            let custom_uuid = custom_uuid.trim();
            if !custom_uuid.is_empty() {
                match uuid::Uuid::parse_str(custom_uuid) {
                    Ok(id) => entry.profile_id = Some(id.to_string()),
                    Err(_) => ui.set_status_text(
                        format!("UUID \"{custom_uuid}\" 格式不对, 已忽略, 按用户名自动生成").into(),
                    ),
                }
            }

            let path = accounts_file_path();
            let mut loaded = hmcl_core::settings::load::<AccountsFile>(
                &path,
                hmcl_core::settings::accounts::SCHEMA_ID,
            );
            loaded.value.upsert_offline_account(&entry);
            if loaded.can_save {
                let _ = hmcl_core::settings::save(
                    &path,
                    hmcl_core::settings::accounts::SCHEMA_ID,
                    &loaded.value,
                );
            }
            set_selected_account(&entry.account_id);
            refresh_accounts(&ui);
            restore_selected_account(&ui);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let authlib_login_task_for_locate = authlib_login_task.clone();
        let pending_authlib_server = pending_authlib_server.clone();
        ui.on_begin_auth_server_locate(move |url| {
            if let Some(previous) = authlib_login_task_for_locate.borrow_mut().take() {
                previous.abort();
            }
            *pending_authlib_server.lock().unwrap() = None;
            let ui_weak = ui_weak.clone();
            let pending_authlib_server = pending_authlib_server.clone();
            let url = url.to_string();
            let task = handle.spawn(async move {
                match hmcl_core::auth::authlib_injector::locate_server(&http_client(), &url).await {
                    Ok(server) => {
                        let name = server.name.clone();
                        let url = server.url.clone();
                        let insecure = url.starts_with("http://");
                        *pending_authlib_server.lock().unwrap() = Some(server);
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_authlib_server_name(name.into());
                            ui.set_authlib_server_url(url.into());
                            ui.set_authlib_server_insecure(insecure);
                            ui.set_authlib_dialog_state(1);
                        });
                    }
                    Err(error) => {
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_authlib_login_message(
                                format!("无法连接认证服务器：{error}").into(),
                            );
                            ui.set_authlib_dialog_state(5);
                        });
                    }
                }
            });
            *authlib_login_task_for_locate.borrow_mut() = Some(task);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let pending_authlib_server = pending_authlib_server.clone();
        ui.on_confirm_auth_server(move || {
            let Some(server) = pending_authlib_server.lock().unwrap().take() else {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_status_text("没有可保存的认证服务器".into());
                }
                return;
            };
            let Some(ui) = ui_weak.upgrade() else { return };
            match save_authlib_injector_server(server) {
                Ok(()) => {
                    refresh_authlib_injector_servers(&ui);
                    ui.set_status_text("认证服务器已添加".into());
                    ui.invoke_close_authlib_dialog();
                }
                Err(error) => {
                    ui.set_authlib_login_message(error.into());
                    ui.set_authlib_dialog_state(5);
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let authlib_login_task_for_login = authlib_login_task.clone();
        let pending_authlib_login = pending_authlib_login.clone();
        ui.on_begin_authlib_login(move |index, login_name, password| {
            if let Some(previous) = authlib_login_task_for_login.borrow_mut().take() {
                previous.abort();
            }
            *pending_authlib_login.lock().unwrap() = None;
            let servers = hmcl_core::settings::load::<AuthlibInjectorServersFile>(
                &authlib_injector_servers_file_path(),
                AUTHLIB_INJECTOR_SERVERS_SCHEMA_ID,
            )
            .value
            .servers;
            let Some(server) = (index >= 0)
                .then(|| servers.get(index as usize))
                .flatten()
                .cloned()
            else {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_authlib_login_message("找不到所选认证服务器".into());
                    ui.set_authlib_dialog_state(5);
                }
                return;
            };
            let login_name = login_name.trim().to_string();
            if !server.non_email_login && !login_name.contains('@') {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_authlib_login_message("该认证服务器要求使用邮箱登录".into());
                    ui.set_authlib_dialog_state(5);
                }
                return;
            }
            let password = password.to_string();
            let ui_weak = ui_weak.clone();
            let pending_authlib_login = pending_authlib_login.clone();
            let task = handle.spawn(async move {
                let client = http_client();
                let server =
                    match hmcl_core::auth::authlib_injector::locate_server(&client, &server.url)
                        .await
                    {
                        Ok(server) => server,
                        Err(error) => {
                            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                                ui.set_authlib_login_password("".into());
                                ui.set_authlib_login_message(
                                    format!("连接认证服务器失败：{error}").into(),
                                );
                                ui.set_authlib_dialog_state(5);
                            });
                            return;
                        }
                    };
                let mut session = match hmcl_core::auth::authlib_injector::authenticate(
                    &client,
                    &server,
                    &login_name,
                    &password,
                )
                .await
                {
                    Ok(session) => session,
                    Err(error) => {
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_authlib_login_password("".into());
                            ui.set_authlib_login_message(format!("登录失败：{error}").into());
                            ui.set_authlib_dialog_state(5);
                        });
                        return;
                    }
                };
                drop(password);

                if session.selected_profile.is_none() && session.available_profiles.len() == 1 {
                    let profile = session.available_profiles[0].clone();
                    session = match hmcl_core::auth::authlib_injector::refresh(
                        &client,
                        &server,
                        &session,
                        Some(&profile),
                    )
                    .await
                    {
                        Ok(session) => session,
                        Err(error) => {
                            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                                ui.set_authlib_login_password("".into());
                                ui.set_authlib_login_message(
                                    format!("选择角色失败：{error}").into(),
                                );
                                ui.set_authlib_dialog_state(5);
                            });
                            return;
                        }
                    };
                }

                if session.selected_profile.is_none() {
                    if session.available_profiles.is_empty() {
                        let _ = ui_weak.upgrade_in_event_loop(|ui| {
                            ui.set_authlib_login_password("".into());
                            ui.set_authlib_login_message("该账户没有可用角色".into());
                            ui.set_authlib_dialog_state(5);
                        });
                        return;
                    }
                    let names = session
                        .available_profiles
                        .iter()
                        .map(|profile| slint::SharedString::from(profile.name.as_str()))
                        .collect::<Vec<_>>();
                    *pending_authlib_login.lock().unwrap() = Some(PendingAuthlibLogin {
                        server,
                        login_name,
                        session,
                    });
                    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                        ui.set_authlib_login_password("".into());
                        ui.set_authlib_profile_names(slint::ModelRc::new(slint::VecModel::from(
                            names,
                        )));
                        ui.set_authlib_dialog_state(4);
                    });
                    return;
                }

                match save_authlib_injector_account(&login_name, &server, session) {
                    Ok(entry) => finish_authlib_login_ui(ui_weak, entry).await,
                    Err(error) => {
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_authlib_login_password("".into());
                            ui.set_authlib_login_message(error.into());
                            ui.set_authlib_dialog_state(5);
                        });
                    }
                }
            });
            *authlib_login_task_for_login.borrow_mut() = Some(task);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let authlib_login_task_for_profile = authlib_login_task.clone();
        let pending_authlib_login = pending_authlib_login.clone();
        ui.on_select_authlib_profile(move |index| {
            if let Some(previous) = authlib_login_task_for_profile.borrow_mut().take() {
                previous.abort();
            }
            let Some(pending) = pending_authlib_login.lock().unwrap().take() else {
                return;
            };
            let Some(profile) = (index >= 0)
                .then(|| pending.session.available_profiles.get(index as usize))
                .flatten()
                .cloned()
            else {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_authlib_login_message("找不到所选角色".into());
                    ui.set_authlib_dialog_state(5);
                }
                return;
            };
            let ui_weak = ui_weak.clone();
            let task = handle.spawn(async move {
                let client = http_client();
                match hmcl_core::auth::authlib_injector::refresh(
                    &client,
                    &pending.server,
                    &pending.session,
                    Some(&profile),
                )
                .await
                {
                    Ok(session) => match save_authlib_injector_account(
                        &pending.login_name,
                        &pending.server,
                        session,
                    ) {
                        Ok(entry) => finish_authlib_login_ui(ui_weak, entry).await,
                        Err(error) => {
                            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                                ui.set_authlib_login_message(error.into());
                                ui.set_authlib_dialog_state(5);
                            });
                        }
                    },
                    Err(error) => {
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_authlib_login_message(format!("选择角色失败：{error}").into());
                            ui.set_authlib_dialog_state(5);
                        });
                    }
                }
            });
            *authlib_login_task_for_profile.borrow_mut() = Some(task);
        });
    }
    {
        let authlib_login_task = authlib_login_task.clone();
        let pending_authlib_server = pending_authlib_server.clone();
        let pending_authlib_login = pending_authlib_login.clone();
        ui.on_cancel_authlib_login(move || {
            if let Some(task) = authlib_login_task.borrow_mut().take() {
                task.abort();
            }
            *pending_authlib_server.lock().unwrap() = None;
            *pending_authlib_login.lock().unwrap() = None;
        });
    }
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let microsoft_login_task_for_start = microsoft_login_task.clone();
        ui.on_begin_microsoft_login(move || {
            if let Some(previous) = microsoft_login_task_for_start.borrow_mut().take() {
                previous.abort();
            }
            let client_id = hmcl_core::auth::microsoft::client_id();
            if client_id.is_empty() {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_microsoft_login_state(4);
                    ui.set_microsoft_login_message("尚未配置 Microsoft Client ID".into());
                }
                return;
            }

            let ui_weak = ui_weak.clone();
            let task = handle.spawn(async move {
                let client = http_client();
                let code = match hmcl_core::auth::microsoft::request_device_code(
                    &client, &client_id,
                )
                .await
                {
                    Ok(code) => code,
                    Err(error) => {
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_microsoft_login_state(3);
                            ui.set_microsoft_login_message(error.to_string().into());
                        });
                        return;
                    }
                };

                let verification_uri = code.verification_uri.clone();
                let user_code = code.user_code.clone();
                let message = code.message.clone();
                let _ = ui_weak.upgrade_in_event_loop({
                    let verification_uri = verification_uri.clone();
                    move |ui| {
                        ui.set_microsoft_login_url(verification_uri.into());
                        ui.set_microsoft_login_code(user_code.into());
                        ui.set_microsoft_login_message(message.into());
                        ui.set_microsoft_login_state(1);
                    }
                });
                let _ = open_url(&verification_uri);

                let authorized_ui = ui_weak.clone();
                let result = hmcl_core::auth::microsoft::authenticate_device_code(
                    &client,
                    &client_id,
                    &code,
                    move || {
                        let _ = authorized_ui.upgrade_in_event_loop(|ui| {
                            ui.set_microsoft_login_state(2);
                        });
                    },
                )
                .await;

                let session = match result {
                    Ok(session) => session,
                    Err(error) => {
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_microsoft_login_state(3);
                            ui.set_microsoft_login_message(error.to_string().into());
                        });
                        return;
                    }
                };
                let _ = cache_microsoft_skin(&client, &session).await;

                let accounts_path = accounts_file_path();
                let mut accounts = hmcl_core::settings::load::<AccountsFile>(
                    &accounts_path,
                    hmcl_core::settings::accounts::SCHEMA_ID,
                );
                let mut entry = MicrosoftAccountEntry::from_session(&session);
                if let Some(existing) = accounts
                    .value
                    .microsoft_accounts()
                    .into_iter()
                    .find(|account| account.profile_id == session.profile_id)
                {
                    entry.account_id = existing.account_id;
                }

                let tokens_path = microsoft_tokens_file_path();
                let mut tokens = hmcl_core::settings::load::<MicrosoftAccountTokensFile>(
                    &tokens_path,
                    MICROSOFT_TOKENS_SCHEMA_ID,
                );
                tokens
                    .value
                    .accounts
                    .insert(entry.account_id.clone(), session);
                let saved = tokens.can_save
                    && accounts.can_save
                    && hmcl_core::settings::save(
                        &tokens_path,
                        MICROSOFT_TOKENS_SCHEMA_ID,
                        &tokens.value,
                    )
                    .is_ok();
                accounts.value.upsert_microsoft_account(&entry);
                if !saved
                    || hmcl_core::settings::save(
                        &accounts_path,
                        hmcl_core::settings::accounts::SCHEMA_ID,
                        &accounts.value,
                    )
                    .is_err()
                {
                    let _ = ui_weak.upgrade_in_event_loop(|ui| {
                        ui.set_microsoft_login_state(3);
                        ui.set_microsoft_login_message("保存微软账户失败".into());
                    });
                    return;
                }

                set_selected_account(&entry.account_id);
                let _ = ui_weak.upgrade_in_event_loop(|ui| {
                    refresh_accounts(&ui);
                    restore_selected_account(&ui);
                    ui.set_status_text("微软账户登录成功".into());
                    ui.set_microsoft_login_state(2);
                });
                tokio::time::sleep(Duration::from_millis(450)).await;
                let _ = ui_weak.upgrade_in_event_loop(|ui| {
                    ui.set_microsoft_dialog_visible(false);
                });
                tokio::time::sleep(Duration::from_millis(200)).await;
                let _ = ui_weak.upgrade_in_event_loop(|ui| {
                    ui.set_microsoft_dialog_mounted(false);
                });
            });
            *microsoft_login_task_for_start.borrow_mut() = Some(task);
        });
    }
    {
        let microsoft_login_task = microsoft_login_task.clone();
        ui.on_cancel_microsoft_login(move || {
            if let Some(task) = microsoft_login_task.borrow_mut().take() {
                task.abort();
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_remove_account(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if index < 0 {
                return;
            }
            let path = accounts_file_path();
            let mut loaded = hmcl_core::settings::load::<AccountsFile>(
                &path,
                hmcl_core::settings::accounts::SCHEMA_ID,
            );
            let accounts = loaded.value.known_accounts();
            let Some(account) = accounts.get(index as usize) else {
                return;
            };
            let account_id = account.account_id().to_string();
            if loaded.value.remove_account(&account_id) && loaded.can_save {
                let _ = hmcl_core::settings::save(
                    &path,
                    hmcl_core::settings::accounts::SCHEMA_ID,
                    &loaded.value,
                );
            }
            let tokens_path = microsoft_tokens_file_path();
            let mut tokens = hmcl_core::settings::load::<MicrosoftAccountTokensFile>(
                &tokens_path,
                MICROSOFT_TOKENS_SCHEMA_ID,
            );
            if tokens.value.accounts.remove(&account_id).is_some() && tokens.can_save {
                let _ = hmcl_core::settings::save(
                    &tokens_path,
                    MICROSOFT_TOKENS_SCHEMA_ID,
                    &tokens.value,
                );
            }
            let authlib_tokens_path = authlib_injector_tokens_file_path();
            let mut authlib_tokens = hmcl_core::settings::load::<AuthlibInjectorAccountTokensFile>(
                &authlib_tokens_path,
                AUTHLIB_INJECTOR_TOKENS_SCHEMA_ID,
            );
            if authlib_tokens.value.accounts.remove(&account_id).is_some()
                && authlib_tokens.can_save
            {
                let _ = hmcl_core::settings::save(
                    &authlib_tokens_path,
                    AUTHLIB_INJECTOR_TOKENS_SCHEMA_ID,
                    &authlib_tokens.value,
                );
            }
            refresh_accounts(&ui);
            restore_selected_account(&ui);
        });
    }

    // 下载页：整份清单只抓一次存在这里, 改筛选条件不重新发请求。
    // 用 Arc<Mutex> 而不是 Rc<RefCell>: 清单是在 tokio worker 线程上抓的, 要跨线程
    // 交给 UI 线程存起来, Rc 不是 Send。锁的持有时间都只有一次赋值/一次遍历, 不会
    // 有争用。
    let remote_manifest: Arc<Mutex<Vec<install::VersionManifestEntry>>> =
        Arc::new(Mutex::new(Vec::new()));
    let loader_versions: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mod_category_state = Arc::new(Mutex::new(ModCategoryState::default()));
    let mod_search_generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mod_detail_state = Arc::new(Mutex::new(ModDetailState::default()));
    let instance_content_cache: InstanceContentCache = Arc::new(Mutex::new(HashMap::new()));
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let remote_manifest = remote_manifest.clone();
        ui.on_refresh_remote_versions(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            ui.set_remote_loading(true);
            ui.set_status_text("正在获取版本清单…".into());

            let ui_weak = ui_weak.clone();
            let remote_manifest = remote_manifest.clone();
            handle.spawn(async move {
                let client = http_client();
                let provider = configured_download_provider(true);
                let fetched = install::fetch_version_manifest(&client, &provider).await;
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_remote_loading(false);
                    match fetched {
                        Ok(manifest) => {
                            let count = manifest.versions.len();
                            let mut game_versions = vec![slint::SharedString::from("全部")];
                            game_versions.extend(
                                manifest
                                    .versions
                                    .iter()
                                    .map(|version| slint::SharedString::from(version.id.clone())),
                            );
                            ui.set_mod_game_version_options(slint::ModelRc::new(
                                slint::VecModel::from(game_versions),
                            ));
                            let mut cache = remote_manifest.lock().unwrap();
                            *cache = manifest.versions;
                            apply_remote_filter(&ui, &cache);
                            ui.set_status_text(format!("版本清单已更新, 共 {count} 个版本").into());
                        }
                        Err(e) => ui.set_status_text(format!("获取版本清单失败: {e}").into()),
                    }
                });
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let remote_manifest = remote_manifest.clone();
        ui.on_remote_filter_changed(move || {
            if let Some(ui) = ui_weak.upgrade() {
                apply_remote_filter(&ui, &remote_manifest.lock().unwrap());
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let loader_versions = loader_versions.clone();
        ui.on_request_loader_versions(move |kind_index, game_version| {
            let Some(kind) = loader_kind(kind_index) else {
                return;
            };
            let game_version = game_version.to_string();
            let ui_weak = ui_weak.clone();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_loader_versions_loading(true);
                ui.set_loader_version_options(slint::ModelRc::new(slint::VecModel::from(Vec::<
                    slint::SharedString,
                >::new(
                ))));
                ui.set_status_text(format!("正在获取 {game_version} 的兼容加载器构建…").into());
            }
            let loader_versions = loader_versions.clone();
            handle.spawn(async move {
                let client = http_client();
                let provider = configured_download_provider(true);
                let result =
                    game_install::fetch_loader_versions(&client, &provider, &game_version, kind)
                        .await;
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    if ui.get_loader_picker_kind() != kind_index
                        || ui.get_install_game_version().as_str() != game_version
                    {
                        return;
                    }
                    ui.set_loader_versions_loading(false);
                    match result {
                        Ok(versions) => {
                            let count = versions.len();
                            let mut cache = loader_versions.lock().unwrap();
                            *cache = versions;
                            apply_loader_filter(&ui, &cache);
                            ui.set_status_text(format!("找到 {count} 个兼容构建").into());
                        }
                        Err(e) => ui.set_status_text(format!("获取加载器构建失败: {e}").into()),
                    }
                });
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let loader_versions = loader_versions.clone();
        ui.on_loader_filter_changed(move || {
            if let Some(ui) = ui_weak.upgrade() {
                apply_loader_filter(&ui, &loader_versions.lock().unwrap());
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let mod_category_state = mod_category_state.clone();
        let mod_search_generation = mod_search_generation.clone();
        ui.on_search_mods(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            start_mod_search(
                &ui,
                &ui_weak,
                &handle,
                &game_dir,
                &mod_category_state,
                &mod_search_generation,
                0,
            );
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let mod_category_state = mod_category_state.clone();
        let mod_search_generation = mod_search_generation.clone();
        ui.on_load_more_mods(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            if ui.get_mod_search_loading() || !ui.get_mod_search_has_more() {
                return;
            }
            let page = ui.get_mod_search_page();
            start_mod_search(
                &ui,
                &ui_weak,
                &handle,
                &game_dir,
                &mod_category_state,
                &mod_search_generation,
                (page + 1) as u64,
            );
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let mod_detail_state = mod_detail_state.clone();
        ui.on_open_mod_project(move |project_id| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let project_id = project_id.to_string();
            let Some(project) = ui
                .get_mod_search_results()
                .iter()
                .find(|row| row.project_id.as_str() == project_id.as_str())
            else {
                return;
            };
            ui.set_mod_detail_project_id(project.project_id.clone());
            ui.set_mod_detail_title(project.title.clone());
            ui.set_mod_detail_description(project.description.clone());
            ui.set_mod_detail_categories(project.categories.clone());
            ui.set_mod_detail_icon(project.icon.clone());
            ui.set_mod_detail_open(true);
            ui.set_mod_detail_loading(true);
            ui.set_mod_detail_versions(slint::ModelRc::new(slint::VecModel::from(Vec::<
                ModVersionRow,
            >::new(
            ))));
            let preferred_game_version = selected_instance_id(&ui)
                .and_then(|instance_id| resolve_instance_context(&game_dir, &instance_id))
                .map(|(version, _)| version);
            handle.spawn(run_mod_detail(
                ui_weak.clone(),
                project_id,
                preferred_game_version,
                mod_detail_state.clone(),
            ));
        });
    }

    {
        let ui_weak = ui.as_weak();
        let mod_detail_state = mod_detail_state.clone();
        ui.on_toggle_mod_version_group(move |game_version| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut state = mod_detail_state.lock().unwrap();
            let game_version = game_version.to_string();
            toggle_mod_detail_group(&mut state.expanded, game_version);
            let rows = mod_detail_rows(&state);
            set_mod_detail_rows(&ui, rows);
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let mod_detail_state = mod_detail_state.clone();
        let install_task = install_task.clone();
        ui.on_install_mod_version(move |version_id, file_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(instance_id) = (ui.get_mod_search_kind() == 3)
                .then(|| ui.get_modpack_import_instance_name().trim().to_string())
                .or_else(|| selected_instance_id(&ui))
            else {
                ui.set_status_text("请先选择一个游戏实例".into());
                return;
            };
            let version_id = version_id.to_string();
            let Some(version) = mod_detail_state
                .lock()
                .unwrap()
                .versions
                .iter()
                .find(|version| version.id == version_id)
                .cloned()
            else {
                ui.set_status_text("找不到选择的项目版本".into());
                return;
            };
            let kind = ui.get_mod_search_kind();
            if kind == 3 {
                if !game_install::is_valid_instance_name(&instance_id) {
                    ui.set_status_text("请输入合法的新实例名称".into());
                    return;
                }
                ui.set_modpack_import_loading(true);
                ui.set_status_text("正在安装整合包…".into());
                let progress = show_modpack_install_progress(&ui, &instance_id, true);
                let ui_weak = ui_weak.clone();
                let game_dir = game_dir.clone();
                let task = handle.spawn(async move {
                    let client = http_client();
                    let provider = configured_download_provider(false);
                    let cache = CacheRepository::new(game_dir.join(".hmcl-rs-cache"));
                    let repo = GameRepository::new(&game_dir);
                    let env = Env {
                        platform: Platform::CURRENT,
                        os_version: "",
                    };
                    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                    let import = modpack::import_from_modrinth_version(
                        &client,
                        &provider,
                        &cache,
                        &repo,
                        &game_dir,
                        &version,
                        &instance_id,
                        env,
                        Some(&tx),
                    );
                    let result =
                        drive_install_progress(ui_weak.clone(), progress, rx, import).await;
                    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                        finish_modpack_import(&ui, &game_dir, &instance_id, result)
                    });
                });
                if let Some(previous) = install_task.borrow_mut().replace(task) {
                    previous.abort();
                }
                return;
            }

            let file_name = file_name.trim().to_string();
            if !valid_download_file_name(&file_name) {
                ui.set_status_text("文件名不能为空，也不能包含路径".into());
                return;
            }
            let version_name = if version.name.is_empty() {
                version.version_number.clone()
            } else {
                version.name.clone()
            };
            let kind_name = match kind {
                1 => "资源包",
                2 => "光影",
                _ => "模组",
            };
            let total = version
                .files
                .iter()
                .find(|file| file.primary)
                .or_else(|| version.files.first())
                .map_or(0, |file| file.size);
            ui.set_status_text(format!("正在下载 {version_name}…").into());
            ui.set_install_title(format!("下载{kind_name}").into());
            ui.set_install_stage_lines(slint::ModelRc::new(slint::VecModel::from(vec![
                InstallStageRow {
                    label: format!("下载 {file_name}").into(),
                    done: 0,
                    total: 1,
                    state: 1,
                    show_count: false,
                },
            ])));
            ui.set_install_active_files(slint::ModelRc::new(slint::VecModel::from(vec![
                InstallFileRow {
                    path: file_name.clone().into(),
                    downloaded: 0.0,
                    total: total as f32,
                },
            ])));
            ui.set_install_speed_text("0 B/s".into());
            ui.set_install_return_to_download_list(true);
            ui.set_show_install_progress(true);

            let ui_weak = ui_weak.clone();
            let game_dir = game_dir.clone();
            let task = handle.spawn(async move {
                let (_, dest_subdir, _) = mod_search_kind_info(kind);
                let repo = GameRepository::new(&game_dir);
                let dest_dir = repo.run_directory(&instance_id).join(dest_subdir);
                let client = http_client();
                let provider = configured_download_provider(false);
                let cache = CacheRepository::new(game_dir.join(".hmcl-rs-cache"));
                let mut downloaded = 0_u64;
                let mut speed_bytes = 0_u64;
                let mut speed_started = Instant::now();
                let progress_ui = ui_weak.clone();
                let progress_file_name = file_name.clone();
                let result = modrinth::install_version_file_as_with_progress(
                    &client,
                    &provider,
                    &cache,
                    &version,
                    &dest_dir,
                    &file_name,
                    move |chunk| {
                        downloaded = downloaded.saturating_add(chunk);
                        speed_bytes = speed_bytes.saturating_add(chunk);
                        let speed =
                            (speed_started.elapsed() >= Duration::from_secs(1)).then(|| {
                                let elapsed = speed_started.elapsed().as_secs_f64().max(0.001);
                                let text = format_speed(speed_bytes as f64 / elapsed);
                                speed_bytes = 0;
                                speed_started = Instant::now();
                                text
                            });
                        let shown_downloaded = if total == 0 {
                            downloaded
                        } else {
                            downloaded.min(total)
                        };
                        let file_name = progress_file_name.clone();
                        let _ = progress_ui.upgrade_in_event_loop(move |ui| {
                            ui.set_install_active_files(slint::ModelRc::new(
                                slint::VecModel::from(vec![InstallFileRow {
                                    path: file_name.into(),
                                    downloaded: shown_downloaded as f32,
                                    total: total as f32,
                                }]),
                            ));
                            if let Some(speed) = speed {
                                ui.set_install_speed_text(speed.into());
                            }
                        });
                    },
                )
                .await;
                let _ = ui_weak.upgrade_in_event_loop(move |ui| match result {
                    Ok(path) => {
                        ui.set_install_stage_lines(slint::ModelRc::new(slint::VecModel::from(
                            vec![InstallStageRow {
                                label: format!("下载 {file_name}").into(),
                                done: 1,
                                total: 1,
                                state: 2,
                                show_count: false,
                            }],
                        )));
                        ui.set_install_active_files(slint::ModelRc::new(slint::VecModel::from(
                            vec![InstallFileRow {
                                path: file_name.into(),
                                downloaded: total as f32,
                                total: total as f32,
                            }],
                        )));
                        ui.set_install_speed_text("完成".into());
                        ui.set_status_text(format!("已安装到 {}", path.display()).into());
                        ui.set_install_succeeded(true);
                    }
                    Err(e) => {
                        ui.set_show_install_progress(false);
                        ui.set_status_text(format!("安装失败: {e}").into());
                    }
                });
            });
            if let Some(previous) = install_task.borrow_mut().replace(task) {
                previous.abort();
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let install_task = install_task.clone();
        ui.on_import_modpack_from_file(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let mrpack_path = ui.get_modpack_import_path().trim().to_string();
            let instance_id = ui.get_modpack_import_instance_name().trim().to_string();
            if mrpack_path.is_empty() {
                ui.set_status_text("请先填整合包文件路径".into());
                return;
            }
            if !game_install::is_valid_instance_name(&instance_id) {
                ui.set_status_text("实例名只能包含字母、数字、点、横线和下划线".into());
                return;
            }

            ui.set_modpack_import_loading(true);
            ui.set_status_text("正在导入整合包…".into());
            let progress = show_modpack_install_progress(&ui, &instance_id, false);

            let ui_weak = ui_weak.clone();
            let game_dir = game_dir.clone();
            let task = handle.spawn(async move {
                let client = http_client();
                let provider = configured_download_provider(false);
                let cache = CacheRepository::new(game_dir.join(".hmcl-rs-cache"));
                let repo = GameRepository::new(&game_dir);
                let env = Env {
                    platform: Platform::CURRENT,
                    os_version: "",
                };
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                let import = modpack::import_mrpack(
                    &client,
                    &provider,
                    &cache,
                    &repo,
                    &game_dir,
                    Path::new(&mrpack_path),
                    &instance_id,
                    env,
                    Some(&tx),
                );
                let result = drive_install_progress(ui_weak.clone(), progress, rx, import).await;
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    finish_modpack_import(&ui, &game_dir, &instance_id, result)
                });
            });
            if let Some(previous) = install_task.borrow_mut().replace(task) {
                previous.abort();
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let install_task = install_task.clone();
        ui.on_import_modpack_from_url(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let url = ui.get_modpack_download_url().trim().to_string();
            let instance_id = ui.get_modpack_import_instance_name().trim().to_string();
            if url.is_empty() {
                ui.set_status_text("请先填下载链接".into());
                return;
            }
            if !game_install::is_valid_instance_name(&instance_id) {
                ui.set_status_text("实例名只能包含字母、数字、点、横线和下划线".into());
                return;
            }

            ui.set_modpack_import_loading(true);
            ui.set_status_text("正在下载整合包…".into());
            let progress = show_modpack_install_progress(&ui, &instance_id, true);

            let ui_weak = ui_weak.clone();
            let game_dir = game_dir.clone();
            let task = handle.spawn(async move {
                let client = http_client();
                let provider = configured_download_provider(false);
                let cache = CacheRepository::new(game_dir.join(".hmcl-rs-cache"));
                let repo = GameRepository::new(&game_dir);
                let env = Env {
                    platform: Platform::CURRENT,
                    os_version: "",
                };
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                let import = modpack::import_from_url(
                    &client,
                    &provider,
                    &cache,
                    &repo,
                    &game_dir,
                    &url,
                    &instance_id,
                    env,
                    Some(&tx),
                );
                let result = drive_install_progress(ui_weak.clone(), progress, rx, import).await;
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    finish_modpack_import(&ui, &game_dir, &instance_id, result)
                });
            });
            if let Some(previous) = install_task.borrow_mut().replace(task) {
                previous.abort();
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let install_task = install_task.clone();
        ui.on_start_install(
            move |version_id, instance_id, loader_kind_index, loader_version| {
                let game_dir = resolve_game_dir();
                let version_id = version_id.to_string();
                let instance_id = instance_id.trim().to_string();
                let loader_version = loader_version.to_string();
                if !game_install::is_valid_instance_name(&instance_id) {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_status_text("实例名只能包含字母、数字、点、横线和下划线".into());
                    }
                    return;
                }
                if loader_kind_index != 0 && loader_version.is_empty() {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_status_text("请先选择一个加载器版本".into());
                    }
                    return;
                }
                if loader_kind_index != 0 && instance_id == version_id {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_status_text("带加载器的实例名不能与原版版本号相同".into());
                    }
                    return;
                }

                let game_dir = game_dir.clone();
                let ui_weak = ui_weak.clone();
                let Some(ui) = ui_weak.upgrade() else { return };
                let mut initial = InstallProgress::new(
                    &version_id,
                    loader_kind(loader_kind_index).map(LoaderKind::display_name),
                );
                let (stages, files, speed) = initial.snapshot();
                ui.set_install_title("安装新游戏".into());
                ui.set_install_stage_lines(slint::ModelRc::new(slint::VecModel::from(stages)));
                ui.set_install_active_files(slint::ModelRc::new(slint::VecModel::from(files)));
                ui.set_install_speed_text(speed.into());
                ui.set_install_return_to_download_list(false);
                ui.set_show_install_progress(true);

                let task = handle.spawn(async move {
                    match install_remote_version(
                        ui_weak.clone(),
                        game_dir.clone(),
                        version_id,
                        instance_id.clone(),
                        loader_kind_index,
                        loader_version,
                    )
                    .await
                    {
                        Ok(()) => {
                            set_selected_instance(&instance_id);
                            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                                ui.set_install_succeeded(true);
                                refresh_instances(&ui, &game_dir, "");
                                restore_selected_instance(&ui);
                            });
                        }
                        Err(e) => {
                            set_status(&ui_weak, format!("安装失败: {e}"));
                            let _ = ui_weak
                                .upgrade_in_event_loop(|ui| ui.set_show_install_progress(false));
                        }
                    }
                });
                if let Some(previous) = install_task.borrow_mut().replace(task) {
                    previous.abort();
                }
            },
        );
    }

    {
        let ui_weak = ui.as_weak();
        let install_task = install_task.clone();
        let handle = handle.clone();
        ui.on_install_instance_loader(move |loader_kind_index, loader_version| {
            let game_dir = resolve_game_dir();
            let Some(kind) = loader_kind(loader_kind_index) else {
                return;
            };
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let loader_version = loader_version.to_string();
            let selection = LoaderSelection {
                kind,
                version: loader_version,
            };
            let mut initial =
                InstallProgress::new(&ui.get_instance_game_version(), Some(kind.display_name()));
            let (stages, files, speed) = initial.snapshot();
            ui.set_install_title(format!("安装 {}", kind.display_name()).into());
            ui.set_install_stage_lines(slint::ModelRc::new(slint::VecModel::from(stages)));
            ui.set_install_active_files(slint::ModelRc::new(slint::VecModel::from(files)));
            ui.set_install_speed_text(speed.into());
            ui.set_install_return_to_download_list(true);
            ui.set_show_install_progress(true);

            let ui_weak = ui_weak.clone();
            let game_dir = game_dir.clone();
            let task = handle.spawn(async move {
                match install_instance_loader(
                    ui_weak.clone(),
                    game_dir.clone(),
                    instance_id,
                    selection,
                )
                .await
                {
                    Ok(new_id) => {
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_settings_instance_id(new_id.clone().into());
                            set_selected_instance(&new_id);
                            ui.set_install_succeeded(true);
                            set_instance_content(&ui, &game_dir, 1);
                            refresh_instances(&ui, &game_dir, &ui.get_filter_text());
                            restore_selected_instance(&ui);
                        });
                    }
                    Err(e) => {
                        set_status(&ui_weak, format!("加载器安装失败: {e}"));
                        let _ =
                            ui_weak.upgrade_in_event_loop(|ui| ui.set_show_install_progress(false));
                    }
                }
            });
            if let Some(previous) = install_task.borrow_mut().replace(task) {
                previous.abort();
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let install_task = install_task.clone();
        ui.on_cancel_install(move || {
            if let Some(task) = install_task.borrow_mut().take() {
                task.abort();
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_java_download_loading(false);
                ui.set_show_install_progress(false);
                ui.set_status_text("安装已取消".into());
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_rename_instance(move |old_id, new_id| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            match rename_instance(&game_dir, &old_id, &new_id) {
                Ok(()) => {
                    ui.set_status_text(format!("已改名为 {new_id}").into());
                    if ui.get_settings_instance_id() == old_id {
                        ui.set_settings_instance_id(new_id.clone());
                    }
                    set_selected_instance(&new_id);
                    refresh_instances(&ui, &game_dir, &ui.get_filter_text());
                    restore_selected_instance(&ui);
                }
                Err(e) => ui.set_status_text(format!("改名失败: {e}").into()),
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_duplicate_instance(move |id| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            match duplicate_instance(&game_dir, &id) {
                Ok(new_id) => {
                    ui.set_status_text(format!("已复制为 {new_id}").into());
                    refresh_instances(&ui, &game_dir, &ui.get_filter_text());
                    restore_selected_instance(&ui);
                }
                Err(e) => ui.set_status_text(format!("复制失败: {e}").into()),
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_delete_instance(move |id| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            match delete_instance(&game_dir, &id) {
                Ok(()) => {
                    ui.set_status_text(format!("已删除 {id}").into());
                    refresh_instances(&ui, &game_dir, &ui.get_filter_text());
                    restore_selected_instance(&ui);
                    if ui.get_settings_instance_id() == id {
                        ui.invoke_show_instance_list();
                    }
                }
                Err(e) => ui.set_status_text(format!("删除失败: {e}").into()),
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_open_run_folder(move |id| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            if let Err(e) = open_run_folder(&game_dir, &id) {
                ui.set_status_text(format!("打开文件夹失败: {e}").into());
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        let instance_content_cache = instance_content_cache.clone();
        ui.on_refresh_instance_content(move |kind| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            if !(2..=4).contains(&kind) {
                set_instance_content(&ui, &game_dir, kind);
                return;
            }
            let instance_id = ui.get_settings_instance_id().to_string();
            let local = local_content_rows(&game_dir, &instance_id, kind);
            let cached = instance_content_cache
                .lock()
                .unwrap()
                .get(&(instance_id.clone(), kind))
                .cloned();
            if let (Ok(local), Some(cached)) = (&local, cached) {
                if cached_content_matches_local(&cached, local) {
                    ui.set_instance_content_loading(false);
                    ui.set_instance_content_delete_confirm_index(-1);
                    ui.set_instance_content_items(slint::ModelRc::new(slint::VecModel::from(
                        materialize_instance_content_rows(cached),
                    )));
                    return;
                }
            }
            set_instance_content(&ui, &game_dir, kind);
            let ui_weak = ui_weak.clone();
            let game_dir = game_dir.clone();
            let instance_content_cache = instance_content_cache.clone();
            handle.spawn(async move {
                let result =
                    instance_content_rows_online_inner(&game_dir, &instance_id, kind, false).await;
                if let Ok((rows, _)) = &result {
                    instance_content_cache
                        .lock()
                        .unwrap()
                        .insert((instance_id.clone(), kind), rows.clone());
                }
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    if ui.get_settings_instance_id().as_str() != instance_id
                        || ui.get_instance_tab() != kind
                    {
                        return;
                    }
                    if let Ok((rows, _)) = result {
                        ui.set_instance_content_items(slint::ModelRc::new(slint::VecModel::from(
                            materialize_instance_content_rows(rows),
                        )));
                    }
                });
            });
        });
    }
    {
        let ui_weak = ui.as_weak();
        let instance_content_cache = instance_content_cache.clone();
        ui.on_toggle_instance_content(move |_kind, file_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            match toggle_instance_mod(&game_dir, &instance_id, &file_name) {
                Err(e) => ui.set_status_text(format!("切换模组状态失败: {e}").into()),
                Ok(target_name) => {
                    let model = ui.get_instance_content_items();
                    if let Some(rows) = model
                        .as_any()
                        .downcast_ref::<slint::VecModel<InstanceContentRow>>()
                    {
                        for index in 0..rows.row_count() {
                            let Some(mut row) = rows.row_data(index) else {
                                continue;
                            };
                            if row.file_name.as_str() == file_name.as_str() {
                                row.file_name = target_name.clone().into();
                                row.enabled = !row.enabled;
                                rows.set_row_data(index, row);
                                if let Some(cached) = instance_content_cache
                                    .lock()
                                    .unwrap()
                                    .get_mut(&(instance_id.clone(), 2))
                                {
                                    if let Some(row) = cached
                                        .iter_mut()
                                        .find(|row| row.file_name == file_name.as_str())
                                    {
                                        row.file_name = target_name.clone();
                                        row.enabled = !row.enabled;
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_delete_instance_content(move |kind, file_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let result = if kind == 1 {
                LoaderKind::from_slug(&file_name)
                    .ok_or_else(|| "未知的加载器".to_string())
                    .and_then(|loader| {
                        game_install::remove_loader(
                            &GameRepository::new(&game_dir),
                            &instance_id,
                            loader,
                        )
                        .map_err(|e| e.to_string())?;
                        sync_instance_loader_name(&game_dir, &instance_id, None)
                    })
            } else {
                delete_instance_content(&game_dir, &instance_id, kind, &file_name)
                    .map(|_| instance_id.clone())
            };
            match result {
                Err(e) => ui.set_status_text(format!("删除失败: {e}").into()),
                Ok(new_id) if kind == 1 => {
                    ui.set_settings_instance_id(new_id.clone().into());
                    set_selected_instance(&new_id);
                    ui.set_status_text("加载器已卸载".into());
                    refresh_instances(&ui, &game_dir, &ui.get_filter_text());
                    restore_selected_instance(&ui);
                }
                Ok(_) => {}
            }
            ui.invoke_refresh_instance_content(kind);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        ui.on_install_local_instance_content(move |kind| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let (title, filter, extensions): (&str, &str, &[&str]) = match kind {
                2 => ("安装本地模组", "模组文件", &["jar", "litemod"]),
                3 => ("安装本地资源包", "资源包", &["zip"]),
                4 => ("安装本地光影", "光影包", &["zip"]),
                _ => return,
            };
            let instance_id = ui.get_settings_instance_id().to_string();
            let initial_dir = GameRepository::new(&game_dir).run_directory(&instance_id);
            let dialog = rfd::AsyncFileDialog::new()
                .set_title(title)
                .set_directory(initial_dir)
                .add_filter(filter, extensions);
            let game_dir = game_dir.clone();
            pick_files_then(
                ui_weak.clone(),
                &handle,
                dialog,
                "已取消本地安装",
                move |ui, sources| match install_local_instance_content(
                    &game_dir,
                    &instance_id,
                    kind,
                    &sources,
                ) {
                    Ok(count) => {
                        ui.set_status_text(format!("已从本地安装 {count} 个文件").into());
                        ui.invoke_refresh_instance_content(kind);
                    }
                    Err(error) => {
                        ui.set_status_text(format!("本地安装失败: {error}").into());
                    }
                },
            );
        });
    }
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        ui.on_check_instance_content_updates(move |kind| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            if !(2..=4).contains(&kind) || ui.get_instance_content_loading() {
                return;
            }
            let instance_id = ui.get_settings_instance_id().to_string();
            ui.set_instance_content_loading(true);
            ui.set_status_text("正在通过 Modrinth 检查更新…".into());
            let ui_weak = ui_weak.clone();
            let game_dir = game_dir.clone();
            handle.spawn(async move {
                let result = instance_content_rows_online(&game_dir, &instance_id, kind).await;
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_instance_content_loading(false);
                    match result {
                        Ok((rows, count)) => {
                            ui.set_instance_content_items(slint::ModelRc::new(
                                slint::VecModel::from(materialize_instance_content_rows(rows)),
                            ));
                            ui.set_status_text(
                                if count == 0 {
                                    "在线更新检查完成，没有发现可用更新".to_string()
                                } else {
                                    format!("在线更新检查完成，发现 {count} 个可用更新")
                                }
                                .into(),
                            );
                        }
                        Err(error) => {
                            ui.set_status_text(format!("检查更新失败: {error}").into());
                        }
                    }
                });
            });
        });
    }
    {
        let ui_weak = ui.as_weak();
        let launch_task = launch_task.clone();
        let launch_cancel = launch_cancel.clone();
        let handle = handle.clone();
        ui.on_launch_world(move |folder_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(account) = selected_account(&ui) else {
                ui.invoke_show_account_page();
                return;
            };
            let instance_id = ui.get_settings_instance_id().to_string();
            let folder_name = folder_name.to_string();
            ui.set_status_text(format!("正在启动 {instance_id} 并进入世界…").into());
            ui.set_show_launch_progress(true);
            push_launch_progress(&ui_weak, 0, format!("正在准备 {instance_id}"));
            let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
            *launch_cancel.borrow_mut() = Some(cancel.clone());
            let task = handle.spawn(launch_instance(
                ui_weak.clone(),
                game_dir.clone(),
                instance_id,
                account,
                None,
                Some(folder_name),
                cancel,
            ));
            if let Some(previous) = launch_task.borrow_mut().replace(task) {
                previous.abort();
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        ui.on_generate_world_launch_script(move |folder_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(account) = selected_account(&ui) else {
                ui.invoke_show_account_page();
                return;
            };
            let instance_id = ui.get_settings_instance_id().to_string();
            let folder_name = folder_name.to_string();
            let dialog = rfd::AsyncFileDialog::new()
                .set_title("保存启动脚本")
                .set_directory(GameRepository::new(&game_dir).run_directory(&instance_id))
                .set_file_name(format!("{folder_name}.bat"))
                .add_filter("Windows 批处理文件", &["bat"]);
            ui.set_status_text("请选择启动脚本保存位置…".into());
            let ui_weak = ui_weak.clone();
            let game_dir = game_dir.clone();
            handle.spawn(async move {
                let Some(file) = dialog.save_file().await else {
                    set_status(&ui_weak, "已取消生成启动脚本".to_string());
                    return;
                };
                let output = with_extension(file.path(), "bat");
                set_status(&ui_weak, format!("正在生成进入 {folder_name} 的启动脚本…"));
                launch_instance(
                    ui_weak,
                    game_dir,
                    instance_id,
                    account,
                    Some(output),
                    Some(folder_name),
                    std::sync::Arc::new(tokio::sync::Notify::new()),
                )
                .await;
            });
        });
    }
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        ui.on_export_world(move |folder_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let world = match open_world(&game_dir, &instance_id, &folder_name) {
                Ok(world) => world,
                Err(e) => {
                    ui.set_status_text(format!("导出世界失败: {e}").into());
                    return;
                }
            };
            let dialog = rfd::AsyncFileDialog::new()
                .set_title("选择该世界的存储位置")
                .set_directory(&game_dir)
                .set_file_name(format!("{folder_name}.zip"))
                .add_filter("世界压缩包", &["zip"]);
            ui.set_status_text("请选择世界压缩包保存位置…".into());
            let ui_weak = ui_weak.clone();
            handle.spawn(async move {
                let Some(file) = dialog.save_file().await else {
                    set_status(&ui_weak, "已取消导出世界".to_string());
                    return;
                };
                let output = with_extension(file.path(), "zip");
                let result = world.export(&output);
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_status_text(
                        match result {
                            Ok(()) => format!("已导出世界到 {}", output.display()),
                            Err(e) => format!("导出世界失败: {e}"),
                        }
                        .into(),
                    );
                });
            });
        });
    }
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        ui.on_import_world(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let dialog = rfd::AsyncFileDialog::new()
                .set_title("选择要添加的世界压缩包")
                .set_directory(&game_dir)
                .add_filter("世界压缩包", &["zip"]);
            ui.set_status_text("请选择要导入的世界压缩包…".into());
            let game_dir = game_dir.clone();
            pick_files_then(
                ui_weak.clone(),
                &handle,
                dialog,
                "已取消导入世界",
                move |ui, paths| {
                    let Some(source) = paths.first() else { return };
                    match import_world(&game_dir, &instance_id, source) {
                        Ok(name) => ui.set_status_text(format!("已导入世界「{name}」").into()),
                        Err(e) => ui.set_status_text(format!("导入世界失败: {e}").into()),
                    }
                    ui.invoke_refresh_instance_content(5);
                },
            );
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_duplicate_world(move |folder_name, new_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let result = open_world(&game_dir, &instance_id, &folder_name)
                .and_then(|world| world.copy_to(&new_name).map_err(|e| e.to_string()));
            ui.set_status_text(
                match result {
                    Ok(_) => format!("已复制世界到「{new_name}」"),
                    Err(e) => format!("复制世界失败: {e}"),
                }
                .into(),
            );
            ui.invoke_refresh_instance_content(5);
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_rename_world(move |folder_name, new_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let result = open_world(&game_dir, &instance_id, &folder_name)
                .and_then(|mut world| world.rename(&new_name).map_err(|e| e.to_string()));
            ui.set_status_text(
                match result {
                    Ok(()) => format!("已重命名为「{new_name}」"),
                    Err(e) => format!("重命名世界失败: {e}"),
                }
                .into(),
            );
            ui.invoke_refresh_instance_content(5);
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_delete_world(move |folder_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let result = open_world(&game_dir, &instance_id, &folder_name)
                .and_then(|world| world.delete().map_err(|e| e.to_string()));
            if let Err(e) = result {
                ui.set_status_text(format!("删除世界失败: {e}").into());
            } else {
                ui.set_status_text(format!("已删除世界「{folder_name}」").into());
            }
            ui.invoke_refresh_instance_content(5);
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_open_world_folder(move |folder_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let result = instance_content_directory(&game_dir, &instance_id, 5)
                .and_then(|saves| direct_content_child(&saves, &folder_name))
                .and_then(|dir| reveal_directory(&dir));
            if let Err(e) = result {
                ui.set_status_text(format!("打开文件夹失败: {e}").into());
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_open_world_detail(move |folder_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            ui.set_world_detail_folder(folder_name);
            ui.set_world_detail_tab(0);
            // 页面切换由 Slint 那边在这个回调返回后自己做（navigate-to 是
            // private function，Rust 调不到）。
            load_world_detail(&ui, &game_dir);
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_copy_world_seed(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let seed = ui.get_world_detail_seed().to_string();
            if seed.is_empty() {
                return;
            }
            #[cfg(windows)]
            match clipboard_win::set_clipboard_string(&seed) {
                Ok(()) => ui.set_status_text("世界种子已复制".into()),
                Err(error) => ui.set_status_text(format!("复制世界种子失败: {error}").into()),
            }
            #[cfg(not(windows))]
            ui.set_status_text("当前平台暂不支持复制世界种子".into());
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_set_world_name(move |name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            edit_world(&ui, &game_dir, |world| world.set_name(&name));
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_set_world_allow_commands(move |value| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            edit_world(&ui, &game_dir, |world| world.set_allow_commands(value));
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_set_world_generate_features(move |value| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            edit_world(&ui, &game_dir, |world| world.set_generate_features(value));
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_set_world_difficulty(move |index| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(difficulty) = hmcl_core::world::Difficulty::from_index(index as usize) else {
                return;
            };
            edit_world(&ui, &game_dir, |world| world.set_difficulty(difficulty));
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_set_world_difficulty_locked(move |value| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            edit_world(&ui, &game_dir, |world| world.set_difficulty_locked(value));
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_set_world_game_type(move |index| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(game_type) = hmcl_core::world::GameType::from_index(index as usize) else {
                return;
            };
            edit_world(&ui, &game_dir, |world| world.set_game_type(game_type));
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_save_world_player_stats(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            // 输入框里正在敲的中间状态（空串、只有一个负号）解析不出来就跳过
            // 这一项，不要把它当成 0 写进存档。
            let health = ui.get_world_player_health().parse::<f32>().ok();
            let food = ui.get_world_player_food().parse::<i32>().ok();
            let saturation = ui.get_world_player_saturation().parse::<f32>().ok();
            let xp = ui.get_world_player_xp().parse::<i32>().ok();
            edit_world(&ui, &game_dir, |world| {
                world.set_player_stats(health, food, saturation, xp)
            });
        });
    }
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        ui.on_change_world_icon(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let dialog = rfd::AsyncFileDialog::new()
                .set_title("选择世界图标")
                .add_filter("PNG 图片", &["png"]);
            let game_dir = game_dir.clone();
            pick_files_then(
                ui_weak.clone(),
                &handle,
                dialog,
                "已取消修改世界图标",
                move |ui, paths| {
                    let Some(source) = paths.first() else { return };
                    let instance_id = ui.get_settings_instance_id().to_string();
                    let folder = ui.get_world_detail_folder().to_string();
                    let result = open_world(&game_dir, &instance_id, &folder)
                        .and_then(|world| world.set_icon(source).map_err(|e| e.to_string()));
                    match result {
                        Ok(()) => {
                            ui.set_status_text("世界图标已更新".into());
                            load_world_detail(ui, &game_dir);
                        }
                        Err(e) => ui.set_status_text(format!("修改世界图标失败: {e}").into()),
                    }
                },
            );
            let _ = ui;
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_clear_world_icon(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let folder = ui.get_world_detail_folder().to_string();
            let result = open_world(&game_dir, &instance_id, &folder)
                .and_then(|world| world.clear_icon().map_err(|e| e.to_string()));
            match result {
                Ok(()) => {
                    ui.set_status_text("已恢复默认世界图标".into());
                    load_world_detail(&ui, &game_dir);
                }
                Err(e) => ui.set_status_text(format!("重置世界图标失败: {e}").into()),
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_refresh_world_backups(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            set_world_backups(&ui, &game_dir);
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_create_world_backup(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let folder = ui.get_world_detail_folder().to_string();
            let backups = instance_backups_directory(&game_dir, &instance_id);
            let result = open_world(&game_dir, &instance_id, &folder)
                .and_then(|world| world.backup(&backups).map_err(|e| e.to_string()));
            ui.set_status_text(
                match result {
                    Ok(path) => format!("已创建备份 {}", path.display()),
                    Err(e) => format!("创建备份失败: {e}"),
                }
                .into(),
            );
            set_world_backups(&ui, &game_dir);
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_delete_world_backup(move |file_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let backups = instance_backups_directory(&game_dir, &instance_id);
            let result = direct_content_child(&backups, &file_name)
                .and_then(|path| std::fs::remove_file(path).map_err(|e| e.to_string()));
            if let Err(e) = result {
                ui.set_status_text(format!("删除备份失败: {e}").into());
            }
            set_world_backups(&ui, &game_dir);
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_open_world_backups_folder(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let dir = instance_backups_directory(&game_dir, ui.get_settings_instance_id().as_str());
            let result = reveal_directory(&dir);
            if let Err(e) = result {
                ui.set_status_text(format!("打开文件夹失败: {e}").into());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_refresh_world_datapacks(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            set_world_datapacks(&ui, &game_dir);
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_toggle_world_datapack(move |file_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let folder = ui.get_world_detail_folder().to_string();
            let result =
                world_datapacks_directory(&game_dir, &instance_id, &folder).and_then(|dir| {
                    let pack = hmcl_core::datapack::list(&dir)
                        .into_iter()
                        .find(|pack| {
                            pack.path
                                .file_name()
                                .is_some_and(|name| name == file_name.as_str())
                        })
                        .ok_or_else(|| format!("找不到数据包 {file_name}"))?;
                    let enabled = !pack.enabled;
                    hmcl_core::datapack::set_enabled(&pack, enabled).map_err(|e| e.to_string())
                });
            if let Err(e) = result {
                ui.set_status_text(format!("切换数据包状态失败: {e}").into());
            }
            set_world_datapacks(&ui, &game_dir);
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_delete_world_datapack(move |file_name| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let folder = ui.get_world_detail_folder().to_string();
            let result =
                world_datapacks_directory(&game_dir, &instance_id, &folder).and_then(|dir| {
                    let path = direct_content_child(&dir, &file_name)?;
                    if path.is_dir() {
                        std::fs::remove_dir_all(path).map_err(|e| e.to_string())
                    } else {
                        std::fs::remove_file(path).map_err(|e| e.to_string())
                    }
                });
            if let Err(e) = result {
                ui.set_status_text(format!("删除数据包失败: {e}").into());
            }
            set_world_datapacks(&ui, &game_dir);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        ui.on_install_world_datapack(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let dialog = rfd::AsyncFileDialog::new()
                .set_title("选择要添加的数据包压缩包")
                .add_filter("数据包", &["zip"]);
            let game_dir = game_dir.clone();
            pick_files_then(
                ui_weak.clone(),
                &handle,
                dialog,
                "已取消添加数据包",
                move |ui, sources| {
                    let instance_id = ui.get_settings_instance_id().to_string();
                    let folder = ui.get_world_detail_folder().to_string();
                    let result = world_datapacks_directory(&game_dir, &instance_id, &folder)
                        .and_then(|dir| {
                            for source in &sources {
                                hmcl_core::datapack::install(&dir, source)
                                    .map_err(|e| e.to_string())?;
                            }
                            Ok(sources.len())
                        });
                    ui.set_status_text(
                        match result {
                            Ok(count) => format!("已添加 {count} 个数据包"),
                            Err(e) => format!("添加数据包失败: {e}"),
                        }
                        .into(),
                    );
                    set_world_datapacks(ui, &game_dir);
                },
            );
            let _ = ui;
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_open_world_datapacks_folder(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let folder = ui.get_world_detail_folder().to_string();
            let result = world_datapacks_directory(&game_dir, &instance_id, &folder)
                .and_then(|dir| reveal_directory(&dir));
            if let Err(e) = result {
                ui.set_status_text(format!("打开文件夹失败: {e}").into());
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_open_instance_content_folder(move |kind| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = ui.get_settings_instance_id().to_string();
            let result = instance_content_directory(&game_dir, &instance_id, kind)
                .and_then(|dir| reveal_directory(&dir));
            if let Err(e) = result {
                ui.set_status_text(format!("打开文件夹失败: {e}").into());
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_open_instance_settings(move |id| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let repo = GameRepository::new(&game_dir);
            let settings = hmcl_core::settings::instance_game_settings::load(&repo, &id);
            let launcher = load_launcher_settings();
            let global_file = hmcl_core::settings::load::<GlobalGameSettingsFile>(
                &game_settings_path(),
                GAME_SETTINGS_SCHEMA_ID,
            )
            .value;
            ui.set_settings_instance_id(id);
            ui.set_instance_tab(0);
            populate_instance_settings_ui(
                &ui,
                &settings,
                selected_global_preset(&launcher, &global_file),
            );
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_save_instance_settings(move || {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let id = ui.get_settings_instance_id().to_string();
            let repo = GameRepository::new(&game_dir);
            let path =
                hmcl_core::settings::instance_game_settings::instance_settings_path(&repo, &id);
            let mut loaded = hmcl_core::settings::load::<
                hmcl_core::settings::instance_game_settings::InstanceGameSettings,
            >(
                &path,
                hmcl_core::settings::instance_game_settings::SCHEMA_ID,
            );
            apply_ui_to_instance_settings(&ui, &mut loaded.value);
            if loaded.can_save {
                if let Err(e) = hmcl_core::settings::save(
                    &path,
                    hmcl_core::settings::instance_game_settings::SCHEMA_ID,
                    &loaded.value,
                ) {
                    ui.set_status_text(format!("保存实例设置失败: {e}").into());
                }
            } else {
                ui.set_status_text(format!("{id} 的设置文件 schema 认不出来, 拒绝覆盖保存").into());
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        ui.on_generate_launch_script(move |instance_id| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(account) = selected_account(&ui) else {
                ui.invoke_show_account_page();
                return;
            };

            let instance_id = instance_id.to_string();
            let repo = GameRepository::new(&game_dir);
            let initial_dir = repo.run_directory(&instance_id);
            let dialog = rfd::AsyncFileDialog::new()
                .set_title("保存启动脚本")
                .set_directory(if initial_dir.is_dir() {
                    &initial_dir
                } else {
                    &game_dir
                })
                .set_file_name(format!("{instance_id}.bat"))
                .add_filter("Windows 批处理文件", &["bat"]);
            let ui_weak = ui_weak.clone();
            let game_dir = game_dir.clone();
            ui.set_status_text("请选择启动脚本保存位置…".into());
            handle.spawn(async move {
                let Some(file) = dialog.save_file().await else {
                    set_status(&ui_weak, "已取消生成启动脚本".to_string());
                    return;
                };
                let output = with_extension(file.path(), "bat");
                set_status(&ui_weak, format!("正在生成 {instance_id} 的启动脚本…"));
                launch_instance(
                    ui_weak,
                    game_dir,
                    instance_id,
                    account,
                    Some(output),
                    None,
                    std::sync::Arc::new(tokio::sync::Notify::new()),
                )
                .await;
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        ui.on_export_instance_modpack(move |instance_id, name, version, summary| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = instance_id.to_string();
            let dialog = rfd::AsyncFileDialog::new()
                .set_title("导出整合包")
                .set_directory(&game_dir)
                .set_file_name(format!("{instance_id}.mrpack"))
                .add_filter("Modrinth 整合包", &["mrpack"]);
            ui.set_status_text("请选择整合包保存位置…".into());
            let ui_weak = ui_weak.clone();
            let game_dir = game_dir.clone();
            let name = name.to_string();
            let version = version.to_string();
            let summary = summary.to_string();
            handle.spawn(async move {
                let Some(file) = dialog.save_file().await else {
                    set_status(&ui_weak, "已取消导出整合包".to_string());
                    return;
                };
                let output = with_extension(file.path(), "mrpack");
                set_status(&ui_weak, format!("正在导出 {instance_id}…"));
                let result = tokio::task::spawn_blocking(move || {
                    let repo = GameRepository::new(game_dir);
                    modpack::export_mrpack(&repo, &instance_id, &output, &name, &version, &summary)
                        .map(|_| output)
                })
                .await;
                match result {
                    Ok(Ok(output)) => {
                        set_status(&ui_weak, format!("整合包已导出到 {}", output.display()))
                    }
                    Ok(Err(error)) => set_status(&ui_weak, format!("导出整合包失败: {error}")),
                    Err(error) => set_status(&ui_weak, format!("导出任务失败: {error}")),
                }
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let handle = handle.clone();
        ui.on_clean_instance_data(move |instance_id, kind| {
            let game_dir = resolve_game_dir();
            let Some(ui) = ui_weak.upgrade() else { return };
            let instance_id = instance_id.to_string();
            let action = match kind {
                0 => "删除共享资源文件",
                1 => "删除共享依赖库",
                _ => "清理游戏日志",
            };
            ui.set_status_text(format!("正在{action}…").into());
            let ui_weak = ui_weak.clone();
            let game_dir = game_dir.clone();
            handle.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let repo = GameRepository::new(game_dir);
                    match kind {
                        0 => repo.clear_shared_assets(&instance_id),
                        1 => repo.clear_shared_libraries(),
                        _ => repo.clean_instance_logs(&instance_id),
                    }
                })
                .await;
                match result {
                    Ok(Ok(())) => set_status(&ui_weak, format!("{action}完成")),
                    Ok(Err(error)) => set_status(&ui_weak, format!("{action}失败: {error}")),
                    Err(error) => set_status(&ui_weak, format!("{action}任务失败: {error}")),
                }
            });
        });
    }

    {
        let launch_cancel = launch_cancel.clone();
        let ui_weak = ui.as_weak();
        ui.on_cancel_launch(move || {
            if let Some(cancel) = launch_cancel.borrow().as_ref() {
                cancel.notify_one();
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_status_text("正在取消启动…".into());
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let launch_task = launch_task.clone();
        let launch_cancel = launch_cancel.clone();
        ui.on_launch_instance(move |instance_id| {
            let game_dir = resolve_game_dir();
            let instance_id = instance_id.to_string();
            let Some(ui) = ui_weak.upgrade() else { return };

            let Some(account) = selected_account(&ui) else {
                ui.invoke_show_account_page();
                return;
            };

            ui.set_status_text(format!("正在启动 {instance_id}...").into());
            ui.set_show_launch_progress(true);
            push_launch_progress(&ui_weak, 0, format!("正在准备 {instance_id}"));
            let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
            *launch_cancel.borrow_mut() = Some(cancel.clone());
            let task = handle.spawn(launch_instance(
                ui_weak.clone(),
                game_dir.clone(),
                instance_id,
                account,
                None,
                None,
                cancel,
            ));
            if let Some(previous) = launch_task.borrow_mut().replace(task) {
                previous.abort();
            }
        });
    }

    ui.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_minecraft_texture_urls_are_always_downloaded_over_https() {
        assert_eq!(
            secure_minecraft_texture_url("http://textures.minecraft.net/texture/0123456789abcdef")
                .as_deref(),
            Some("https://textures.minecraft.net/texture/0123456789abcdef")
        );
        assert_eq!(
            secure_minecraft_texture_url("https://example.com/not-a-minecraft-skin"),
            None
        );
    }

    #[test]
    fn minecraft_directory_names_use_the_profile_folder() {
        assert_eq!(
            suggested_game_directory_name(Path::new("profiles/Vanilla/.minecraft")),
            "Vanilla"
        );
        assert_eq!(
            suggested_game_directory_name(Path::new("profiles/Modded")),
            "Modded"
        );
    }

    #[test]
    fn crash_log_buffer_keeps_only_the_most_recent_lines() {
        let logs = Arc::new(Mutex::new(VecDeque::new()));
        for index in 0..605 {
            retain_recent_game_log(&logs, &format!("line-{index}"));
        }
        let logs = logs.lock().unwrap();
        assert_eq!(logs.len(), 600);
        assert_eq!(logs.front().map(String::as_str), Some("line-5"));
        assert_eq!(logs.back().map(String::as_str), Some("line-604"));
    }

    #[test]
    fn crash_log_levels_recognize_minecraft_and_java_output() {
        assert_eq!(crash_log_level("[main/INFO] Starting Minecraft"), 2);
        assert_eq!(crash_log_level("[Render thread/WARN] Missing texture"), 3);
        assert_eq!(crash_log_level("java.lang.NullPointerException"), 4);
        assert_eq!(crash_log_level("Loading resource packs"), 0);
    }

    #[test]
    fn minecraft_wiki_links_distinguish_releases_and_snapshots() {
        let release = minecraft_wiki_url("1.21.8");
        assert!(release.contains("/w/Java%E7%89%881.21.8"));
        assert!(!release.contains("/w//"));
        assert!(minecraft_wiki_url("26w14a").contains("/26w14a?"));
    }

    #[test]
    fn modpack_titles_become_valid_instance_names() {
        assert_eq!(
            suggested_instance_name("Fabulously Optimized 6.0"),
            "Fabulously-Optimized-6.0"
        );
        assert_eq!(suggested_instance_name("整合包"), "整合包");
        assert!(game_install::is_valid_instance_name(
            &suggested_instance_name("Better MC [FABRIC]")
        ));
    }

    #[test]
    fn automatic_source_selection_matches_hmcl_region_policy() {
        assert!(prefers_mirror_for_environment(
            9 * 3600 + 1800,
            Some("Asia/Shanghai"),
            None,
            None
        ));
        assert!(prefers_mirror_for_environment(
            8 * 3600,
            None,
            Some("zh_CN.UTF-8"),
            None
        ));
        assert!(prefers_mirror_for_environment(
            8 * 3600,
            None,
            None,
            Some(45)
        ));
        assert!(!prefers_mirror_for_environment(
            9 * 3600 + 1800,
            None,
            Some("zh_CN.UTF-8"),
            Some(45)
        ));
    }

    #[test]
    fn april_fools_versions_are_separate_from_regular_snapshots() {
        let april_fools = install::VersionManifestEntry {
            id: "26w14a".to_string(),
            url: String::new(),
            release_type: Some(hmcl_core::version::ReleaseType::Snapshot),
            release_time: None,
            sha1: None,
        };
        let snapshot = install::VersionManifestEntry {
            id: "26.2-snapshot-1".to_string(),
            ..april_fools.clone()
        };

        assert!(version_type_matches(&april_fools, 2));
        assert!(!version_type_matches(&april_fools, 1));
        assert_eq!(version_type_label(&april_fools), "愚人节版");
        assert!(version_type_matches(&snapshot, 1));
        assert!(!version_type_matches(&snapshot, 2));
    }

    #[test]
    fn automatic_download_threads_match_hmcl_cpu_times_four_capped_at_64() {
        assert_eq!(automatic_download_concurrency(1), 4);
        assert_eq!(automatic_download_concurrency(8), 32);
        assert_eq!(automatic_download_concurrency(20), 64);
        assert_eq!(automatic_download_concurrency(usize::MAX), 64);
    }

    #[test]
    fn loader_names_replace_old_suffixes_without_repeating_them() {
        assert_eq!(
            loader_instance_name("26.2", Some(LoaderKind::NeoForge)),
            "26.2-neoforge"
        );
        assert_eq!(
            loader_instance_name("26.2-neoforge", Some(LoaderKind::Fabric)),
            "26.2-fabric"
        );
        assert_eq!(
            loader_instance_name("26.2-neoforge-neoforge", Some(LoaderKind::NeoForge)),
            "26.2-neoforge"
        );
        assert_eq!(loader_instance_name("26.2-neoforge-2", None), "26.2");
    }

    #[test]
    fn removing_a_loader_promotes_its_hidden_game_parent_without_a_number_suffix() {
        let root =
            std::env::temp_dir().join(format!("hmcl-gui-loader-name-{}", uuid::Uuid::now_v7()));
        let repo = GameRepository::new(&root);
        let mut parent = hmcl_core::version::Version::new("26.2");
        parent.hidden = Some(true);
        repo.save_version_json(&parent).unwrap();

        let mut instance = hmcl_core::version::Version::new("26.2-neoforge");
        instance.inherits_from = Some("26.2".to_string());
        instance.patches = Some(Vec::new());
        repo.save_version_json(&instance).unwrap();
        let settings = repo.version_root(&instance.id).join(".hmcl/config");
        std::fs::create_dir_all(&settings).unwrap();
        std::fs::write(settings.join("instance-game-settings.json"), b"settings").unwrap();

        let new_id = sync_instance_loader_name(&root, &instance.id, None).unwrap();

        assert_eq!(new_id, "26.2");
        assert!(!repo.version_root(&instance.id).exists());
        assert!(!repo.load_all_versions()["26.2"].is_hidden());
        assert_eq!(
            std::fs::read(
                repo.version_root("26.2")
                    .join(".hmcl/config/instance-game-settings.json")
            )
            .unwrap(),
            b"settings"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn instance_icon_loader_is_read_from_the_current_patch() {
        let mut instance = hmcl_core::version::Version::new("26.2-neoforge");
        instance.patches = Some(vec![hmcl_core::version::Version::new("neoforge")]);
        let versions = std::collections::HashMap::from([(instance.id.clone(), instance.clone())]);
        assert_eq!(
            instance_loader_kind(&instance, &versions),
            Some(LoaderKind::NeoForge)
        );
    }

    #[test]
    fn mod_version_groups_behave_like_an_accordion() {
        let mut expanded = BTreeSet::from(["1.20.1".to_string()]);
        toggle_mod_detail_group(&mut expanded, "1.21.1".to_string());
        assert_eq!(expanded, BTreeSet::from(["1.21.1".to_string()]));
        toggle_mod_detail_group(&mut expanded, "1.21.1".to_string());
        assert!(expanded.is_empty());
    }

    #[test]
    fn local_mod_management_lists_and_toggles_direct_children_only() {
        let root =
            std::env::temp_dir().join(format!("hmcl-gui-local-content-{}", uuid::Uuid::now_v7()));
        let mods = root.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("enabled.jar"), b"jar").unwrap();
        std::fs::write(mods.join("disabled.jar.disabled"), b"jar").unwrap();

        let rows = local_content_rows(&root, "test", 2).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .any(|row| row.name == "enabled.jar" && row.enabled));
        assert!(rows
            .iter()
            .any(|row| row.name == "disabled.jar" && !row.enabled));

        assert_eq!(
            toggle_instance_mod(&root, "test", "enabled.jar").unwrap(),
            "enabled.jar.disabled"
        );
        assert!(mods.join("enabled.jar.disabled").is_file());
        assert!(direct_content_child(&mods, "../outside.jar").is_err());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn instance_content_cache_only_matches_the_same_local_files() {
        let cached = vec![PendingInstanceContentRow {
            file_name: "example.jar".to_string(),
            name: "Example".to_string(),
            detail: "1.0 MiB".to_string(),
            enabled: true,
            directory: false,
            icon_path: Some(PathBuf::from("cached-icon")),
        }];
        let mut local = vec![InstanceContentRow {
            file_name: "example.jar".into(),
            name: "example.jar".into(),
            detail: "1.0 MiB".into(),
            enabled: true,
            directory: false,
            online_icon: false,
            icon: Default::default(),
        }];
        assert!(cached_content_matches_local(&cached, &local));
        local[0].file_name = "example.jar.disabled".into();
        assert!(!cached_content_matches_local(&cached, &local));
    }

    #[test]
    fn install_progress_aggregates_stage_and_file_events() {
        let mut progress = InstallProgress::new("1.20.1", Some("Forge"));
        progress.apply(ProgressEvent::LoaderStarted {
            name: "Forge".to_string(),
        });
        assert_eq!(progress.stages[0].state, 2);
        assert_eq!(progress.stages[1].label, "安装 Forge");
        assert_eq!(progress.stages[1].state, 1);
        progress.apply(ProgressEvent::LoaderFinished);
        progress.apply(ProgressEvent::StageStarted {
            stage: InstallStage::Libraries,
            total: 2,
        });
        progress.apply(ProgressEvent::Bytes {
            path: PathBuf::from("libraries/example.jar"),
            chunk_bytes: 100,
            total_bytes: Some(100),
        });
        progress.apply(ProgressEvent::TaskDone {
            stage: InstallStage::Libraries,
        });

        assert_eq!(progress.stages[0].state, 2);
        assert_eq!(progress.stages[1].state, 2);
        assert_eq!(progress.stages[2].done, 1);
        assert_eq!(progress.stages[2].total, 2);

        let (_, files, speed) = progress.snapshot();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path.as_str(), "example.jar");
        assert_eq!(files[0].downloaded, 100.0);
        assert_eq!(files[0].total, 100.0);
        assert_ne!(speed, "0 B/s");

        progress.apply(ProgressEvent::StageStarted {
            stage: InstallStage::AssetObjects,
            total: 3,
        });
        assert_eq!(progress.stages[2].state, 2);
        assert_eq!(progress.status_snapshot().1.len(), 1);
    }
}
