use serde::{Deserialize, Serialize};

use crate::install::GameRepository;

pub const SCHEMA_ID: &str = "instance-game-settings";

// 对应 Java `GameSettings.PROPERTY_*` 字符串常量——`overrideProperties` 数组里
// 存的就是这些字面量，必须跟 Java 版一字不差，否则跨语言共用同一个
// `.minecraft` 目录时会读不出对方写的覆盖标记。
pub const PROPERTY_JAVA_TYPE: &str = "javaType";
pub const PROPERTY_CUSTOM_JAVA_VERSION: &str = "customJavaVersion";
pub const PROPERTY_CUSTOM_JAVA_PATH: &str = "customJavaPath";
pub const PROPERTY_JVM_OPTIONS: &str = "jvmOptions";
pub const PROPERTY_NO_JVM_OPTIONS: &str = "noJVMOptions";
pub const PROPERTY_AUTO_MEMORY: &str = "autoMemory";
pub const PROPERTY_MIN_MEMORY: &str = "minMemory";
pub const PROPERTY_MAX_MEMORY: &str = "maxMemory";
pub const PROPERTY_WINDOW_TYPE: &str = "windowType";
pub const PROPERTY_WIDTH: &str = "width";
pub const PROPERTY_HEIGHT: &str = "height";
pub const PROPERTY_RUNNING_DIRECTORY: &str = "runningDirectory";
pub const PROPERTY_GAME_ARGUMENTS: &str = "gameArguments";
pub const PROPERTY_GRAPHICS_BACKEND: &str = "graphicsBackend";
pub const PROPERTY_QUICK_PLAY: &str = "quickPlay";
pub const PROPERTY_QUICK_PLAY_MULTIPLAYER: &str = "quickPlayMultiplayer";
pub const PROPERTY_QUICK_PLAY_SINGLEPLAYER: &str = "quickPlaySingleplayer";
pub const PROPERTY_QUICK_PLAY_REALMS: &str = "quickPlayRealms";
pub const PROPERTY_NO_OPTIMIZING_JVM_OPTIONS: &str = "noOptimizingJVMOptions";
pub const PROPERTY_NOT_CHECK_JVM: &str = "notCheckJVM";
pub const PROPERTY_ENABLE_DEBUG_LOG_OUTPUT: &str = "enableDebugLogOutput";
pub const PROPERTY_PROCESS_PRIORITY: &str = "processPriority";
pub const PROPERTY_LAUNCHER_VISIBILITY: &str = "launcherVisibility";
pub const PROPERTY_ENVIRONMENT_VARIABLES: &str = "environmentVariables";
pub const PROPERTY_COMMAND_WRAPPER: &str = "commandWrapper";
pub const PROPERTY_PRE_LAUNCH_COMMAND: &str = "preLaunchCommand";
pub const PROPERTY_POST_EXIT_COMMAND: &str = "postExitCommand";
pub const PROPERTY_PERM_SIZE: &str = "permSize";
pub const PROPERTY_USE_CUSTOM_NATIVES: &str = "useCustomNatives";
pub const PROPERTY_NATIVES_DIRECTORY: &str = "nativesDirectory";

pub fn instance_settings_path(repo: &GameRepository, id: &str) -> std::path::PathBuf {
    repo.version_root(id)
        .join(".hmcl")
        .join("config")
        .join("instance-game-settings.json")
}

pub fn load(repo: &GameRepository, id: &str) -> InstanceGameSettings {
    crate::settings::load::<InstanceGameSettings>(&instance_settings_path(repo, id), SCHEMA_ID)
        .value
}

pub fn auto_allocated_memory_mb() -> u32 {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    (auto_allocated_memory_bytes(system.available_memory()) / 1024 / 1024)
        .clamp(256, u32::MAX as u64) as u32
}

fn auto_allocated_memory_bytes(available: u64) -> u64 {
    let reserve = 512 * 1024 * 1024;
    let usable = available.saturating_sub(reserve);
    let usable = if usable == 0 { available } else { usable };
    let threshold = 8_u64 * 1024 * 1024 * 1024;
    let suggested = if usable <= threshold {
        usable.saturating_mul(4) / 5
    } else {
        (threshold.saturating_mul(4) / 5 + (usable - threshold) / 5)
            .min(16_u64 * 1024 * 1024 * 1024)
    };
    suggested.max(256 * 1024 * 1024)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JavaSelectionType {
    #[serde(rename = "AUTO")]
    Auto,
    #[serde(rename = "CUSTOM")]
    Custom,
    #[serde(rename = "VERSION")]
    Version,
    #[serde(rename = "DETECTED")]
    Detected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowType {
    #[serde(rename = "WINDOWED")]
    Windowed,
    #[serde(rename = "FULLSCREEN")]
    Fullscreen,
    #[serde(rename = "MAXIMIZED")]
    Maximized,
}

/// 对应 Java `ProcessPriority`。真实 Java 在 Windows 上这个设置是**没有实现的**
/// ——`DefaultLauncher.generateCommandLine` 里设置 Windows 进程优先级那段代码
/// 整段被注释掉了，只有 Linux/macOS/BSD 用 `nice -n` 生效。这里照抄这个不对称：
/// 建模+持久化这个字段，但 `hmcl_core::launch::ProcessPriority` 在 windows-gnu
/// 上同样不接到任何 OS 调用（见 `launch` 模块），不是漏做。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProcessPriority {
    Low,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
}

impl From<ProcessPriority> for crate::launch::ProcessPriority {
    fn from(value: ProcessPriority) -> Self {
        match value {
            ProcessPriority::Low => crate::launch::ProcessPriority::Low,
            ProcessPriority::BelowNormal => crate::launch::ProcessPriority::BelowNormal,
            ProcessPriority::Normal => crate::launch::ProcessPriority::Normal,
            ProcessPriority::AboveNormal => crate::launch::ProcessPriority::AboveNormal,
            ProcessPriority::High => crate::launch::ProcessPriority::High,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LauncherVisibility {
    Close,
    #[serde(alias = "HIDE", alias = "HIDE_AND_REOPEN")]
    Minimize,
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuickPlayType {
    #[serde(rename = "NONE")]
    None,
    #[serde(rename = "MULTIPLAYER")]
    Multiplayer,
    #[serde(rename = "SINGLEPLAYER")]
    Singleplayer,
    #[serde(rename = "REALMS")]
    Realms,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InstanceGameSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(
        default,
        rename = "overrideProperties",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub override_properties: Vec<String>,

    #[serde(default, rename = "javaType", skip_serializing_if = "Option::is_none")]
    pub java_type: Option<JavaSelectionType>,
    #[serde(
        default,
        rename = "customJavaVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub custom_java_version: Option<String>,
    #[serde(
        default,
        rename = "customJavaPath",
        skip_serializing_if = "Option::is_none"
    )]
    pub custom_java_path: Option<String>,

    #[serde(
        default,
        rename = "jvmOptions",
        skip_serializing_if = "Option::is_none"
    )]
    pub jvm_options: Option<String>,
    #[serde(
        default,
        rename = "noJVMOptions",
        skip_serializing_if = "Option::is_none"
    )]
    pub no_jvm_options: Option<bool>,
    #[serde(
        default,
        rename = "autoMemory",
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_memory: Option<bool>,
    #[serde(default, rename = "minMemory", skip_serializing_if = "Option::is_none")]
    pub min_memory: Option<u32>,
    #[serde(default, rename = "maxMemory", skip_serializing_if = "Option::is_none")]
    pub max_memory: Option<u32>,

    #[serde(
        default,
        rename = "windowType",
        skip_serializing_if = "Option::is_none"
    )]
    pub window_type: Option<WindowType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,

    #[serde(
        default,
        rename = "runningDirectory",
        skip_serializing_if = "Option::is_none"
    )]
    pub running_directory: Option<String>,

    #[serde(
        default,
        rename = "gameArguments",
        skip_serializing_if = "Option::is_none"
    )]
    pub game_arguments: Option<String>,
    #[serde(
        default,
        rename = "graphicsBackend",
        skip_serializing_if = "Option::is_none"
    )]
    pub graphics_backend: Option<crate::launch::GraphicsApi>,

    #[serde(default, rename = "quickPlay", skip_serializing_if = "Option::is_none")]
    pub quick_play: Option<QuickPlayType>,
    #[serde(
        default,
        rename = "quickPlayMultiplayer",
        skip_serializing_if = "Option::is_none"
    )]
    pub quick_play_multiplayer: Option<String>,
    #[serde(
        default,
        rename = "quickPlaySingleplayer",
        skip_serializing_if = "Option::is_none"
    )]
    pub quick_play_singleplayer: Option<String>,
    #[serde(
        default,
        rename = "quickPlayRealms",
        skip_serializing_if = "Option::is_none"
    )]
    pub quick_play_realms: Option<String>,

    #[serde(
        default,
        rename = "noOptimizingJVMOptions",
        skip_serializing_if = "Option::is_none"
    )]
    pub no_optimizing_jvm_options: Option<bool>,
    #[serde(
        default,
        rename = "notCheckJVM",
        skip_serializing_if = "Option::is_none"
    )]
    pub not_check_jvm: Option<bool>,
    #[serde(
        default,
        rename = "enableDebugLogOutput",
        skip_serializing_if = "Option::is_none"
    )]
    pub enable_debug_log_output: Option<bool>,
    #[serde(
        default,
        rename = "processPriority",
        skip_serializing_if = "Option::is_none"
    )]
    pub process_priority: Option<ProcessPriority>,
    #[serde(
        default,
        rename = "launcherVisibility",
        skip_serializing_if = "Option::is_none"
    )]
    pub launcher_visibility: Option<LauncherVisibility>,
    #[serde(
        default,
        rename = "environmentVariables",
        skip_serializing_if = "Option::is_none"
    )]
    pub environment_variables: Option<String>,
    #[serde(
        default,
        rename = "commandWrapper",
        skip_serializing_if = "Option::is_none"
    )]
    pub command_wrapper: Option<String>,
    #[serde(
        default,
        rename = "preLaunchCommand",
        skip_serializing_if = "Option::is_none"
    )]
    pub pre_launch_command: Option<String>,
    #[serde(
        default,
        rename = "postExitCommand",
        skip_serializing_if = "Option::is_none"
    )]
    pub post_exit_command: Option<String>,
    #[serde(default, rename = "permSize", skip_serializing_if = "Option::is_none")]
    pub permanent_generation_size: Option<u32>,
    #[serde(
        default,
        rename = "useCustomNatives",
        skip_serializing_if = "Option::is_none"
    )]
    pub use_custom_natives: Option<bool>,
    #[serde(
        default,
        rename = "nativesDirectory",
        skip_serializing_if = "Option::is_none"
    )]
    pub natives_directory: Option<String>,

    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl InstanceGameSettings {
    pub fn is_overridden(&self, property: &str) -> bool {
        self.override_properties.iter().any(|p| p == property)
    }

    pub fn set_overridden(&mut self, property: &str) {
        if !self.is_overridden(property) {
            self.override_properties.push(property.to_string());
        }
    }

    pub fn effective_max_memory(&self, default_mb: u32) -> u32 {
        if self.is_overridden(PROPERTY_MAX_MEMORY) {
            self.max_memory.unwrap_or(default_mb)
        } else {
            default_mb
        }
    }

    pub fn effective_auto_memory(&self, default: bool) -> bool {
        if self.is_overridden(PROPERTY_AUTO_MEMORY) {
            self.auto_memory.unwrap_or(default)
        } else {
            default
        }
    }

    pub fn effective_jvm_options(&self) -> Option<&str> {
        if self.is_overridden(PROPERTY_JVM_OPTIONS) {
            self.jvm_options.as_deref()
        } else {
            None
        }
    }

    pub fn effective_no_jvm_options(&self) -> bool {
        if self.is_overridden(PROPERTY_NO_JVM_OPTIONS) {
            self.no_jvm_options.unwrap_or(false)
        } else {
            false
        }
    }

    pub fn effective_no_optimizing_jvm_options(&self) -> bool {
        if self.is_overridden(PROPERTY_NO_OPTIMIZING_JVM_OPTIONS) {
            self.no_optimizing_jvm_options.unwrap_or(false)
        } else {
            false
        }
    }

    pub fn effective_min_memory(&self) -> Option<u32> {
        if self.is_overridden(PROPERTY_MIN_MEMORY) {
            self.min_memory
        } else {
            None
        }
    }

    pub fn effective_metaspace(&self) -> Option<u32> {
        if self.is_overridden(PROPERTY_PERM_SIZE) {
            self.permanent_generation_size
        } else {
            None
        }
    }

    /// `(width, height, fullscreen)`。`WindowType::Maximized` 在这个 port 里退化成
    /// "按给定分辨率正常窗口化启动"——真正的"启动后把窗口最大化"需要拿到游戏
    /// 进程自己创建的窗口句柄再调 OS API（`ShowWindow(hwnd, SW_MAXIMIZE)`），
    /// 这个 port 没有做那一层（游戏窗口不是我们创建的，没有现成句柄）。
    pub fn effective_window(&self, default_width: i32, default_height: i32) -> (i32, i32, bool) {
        let width = if self.is_overridden(PROPERTY_WIDTH) {
            self.width.map(|w| w as i32).unwrap_or(default_width)
        } else {
            default_width
        };
        let height = if self.is_overridden(PROPERTY_HEIGHT) {
            self.height.map(|h| h as i32).unwrap_or(default_height)
        } else {
            default_height
        };
        let fullscreen = self.is_overridden(PROPERTY_WINDOW_TYPE)
            && self.window_type == Some(WindowType::Fullscreen);
        (width, height, fullscreen)
    }

    pub fn effective_quick_play_option(&self) -> Option<crate::launch::QuickPlayOption> {
        if !self.is_overridden(PROPERTY_QUICK_PLAY) {
            return None;
        }
        match self.quick_play {
            Some(QuickPlayType::Multiplayer) => self
                .quick_play_multiplayer
                .clone()
                .filter(|s| !s.is_empty())
                .map(|server_ip| crate::launch::QuickPlayOption::MultiPlayer { server_ip }),
            Some(QuickPlayType::Singleplayer) => self
                .quick_play_singleplayer
                .clone()
                .filter(|s| !s.is_empty())
                .map(
                    |world_folder_name| crate::launch::QuickPlayOption::SinglePlayer {
                        world_folder_name,
                    },
                ),
            Some(QuickPlayType::Realms) => self
                .quick_play_realms
                .clone()
                .filter(|s| !s.is_empty())
                .map(|realm_id| crate::launch::QuickPlayOption::Realm { realm_id }),
            Some(QuickPlayType::None) | None => None,
        }
    }

    pub fn effective_game_arguments(&self) -> Option<&str> {
        if self.is_overridden(PROPERTY_GAME_ARGUMENTS) {
            self.game_arguments.as_deref()
        } else {
            None
        }
    }

    pub fn effective_graphics_backend(&self) -> crate::launch::GraphicsApi {
        if self.is_overridden(PROPERTY_GRAPHICS_BACKEND) {
            self.graphics_backend.unwrap_or_default()
        } else {
            Default::default()
        }
    }

    pub fn effective_wrapper(&self) -> Option<&str> {
        if self.is_overridden(PROPERTY_COMMAND_WRAPPER) {
            self.command_wrapper
                .as_deref()
                .filter(|s| !s.trim().is_empty())
        } else {
            None
        }
    }

    pub fn effective_process_priority(&self) -> crate::launch::ProcessPriority {
        if self.is_overridden(PROPERTY_PROCESS_PRIORITY) {
            self.process_priority.map(Into::into).unwrap_or_default()
        } else {
            Default::default()
        }
    }

    pub fn effective_debug_log_output(&self) -> bool {
        if self.is_overridden(PROPERTY_ENABLE_DEBUG_LOG_OUTPUT) {
            self.enable_debug_log_output.unwrap_or(false)
        } else {
            false
        }
    }

    pub fn effective_use_custom_natives(&self) -> bool {
        if self.is_overridden(PROPERTY_USE_CUSTOM_NATIVES) {
            self.use_custom_natives.unwrap_or(false)
        } else {
            false
        }
    }

    pub fn effective_natives_directory(&self) -> Option<&str> {
        if self.is_overridden(PROPERTY_NATIVES_DIRECTORY) {
            self.natives_directory.as_deref()
        } else {
            None
        }
    }

    pub fn effective_environment_variables(&self) -> Vec<(String, String)> {
        if !self.is_overridden(PROPERTY_ENVIRONMENT_VARIABLES) {
            return Vec::new();
        }
        let Some(raw) = self.environment_variables.as_deref() else {
            return Vec::new();
        };
        raw.lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .filter(|(k, _)| !k.is_empty())
            .collect()
    }

    pub fn effective_pre_launch_command(&self) -> Option<&str> {
        if self.is_overridden(PROPERTY_PRE_LAUNCH_COMMAND) {
            self.pre_launch_command
                .as_deref()
                .filter(|s| !s.trim().is_empty())
        } else {
            None
        }
    }

    pub fn effective_post_exit_command(&self) -> Option<&str> {
        if self.is_overridden(PROPERTY_POST_EXIT_COMMAND) {
            self.post_exit_command
                .as_deref()
                .filter(|s| !s.trim().is_empty())
        } else {
            None
        }
    }

    pub fn effective_launcher_visibility(&self) -> LauncherVisibility {
        if self.is_overridden(PROPERTY_LAUNCHER_VISIBILITY) {
            self.launcher_visibility.unwrap_or(LauncherVisibility::Keep)
        } else {
            LauncherVisibility::Keep
        }
    }

    /// 算出这个实例真正应该用的运行目录。对应 Java
    /// `HMCLGameRepository.getRunDirectory`：`is_modpack` 的实例无条件用自己的
    /// 目录，不管 `runningDirectory` 设了什么；否则委托给
    /// [`GameRepository::run_directory_isolated`]。
    pub fn run_directory(
        &self,
        repo: &GameRepository,
        id: &str,
        is_modpack: bool,
    ) -> std::path::PathBuf {
        let overridden = self.is_overridden(PROPERTY_RUNNING_DIRECTORY);
        let running_directory = self.running_directory.as_deref().unwrap_or("");
        repo.run_directory_isolated(id, overridden, running_directory, is_modpack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_memory_matches_hmcl_piecewise_formula() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(
            auto_allocated_memory_bytes(256 * 1024 * 1024),
            256 * 1024 * 1024
        );
        assert_eq!(
            auto_allocated_memory_bytes(8 * GIB + 512 * 1024 * 1024),
            8 * GIB * 4 / 5
        );
        assert_eq!(auto_allocated_memory_bytes(100 * GIB), 16 * GIB);
    }

    #[test]
    fn overridden_property_wins_over_default() {
        let mut settings = InstanceGameSettings::default();
        assert_eq!(
            settings.effective_max_memory(2048),
            2048,
            "not overridden: falls back to caller's default"
        );

        settings.max_memory = Some(8192);
        settings.set_overridden(PROPERTY_MAX_MEMORY);
        assert_eq!(
            settings.effective_max_memory(2048),
            8192,
            "overridden: instance's own value wins"
        );
    }

    #[test]
    fn set_overridden_does_not_duplicate() {
        let mut settings = InstanceGameSettings::default();
        settings.set_overridden(PROPERTY_MAX_MEMORY);
        settings.set_overridden(PROPERTY_MAX_MEMORY);
        assert_eq!(
            settings.override_properties,
            vec![PROPERTY_MAX_MEMORY.to_string()]
        );
    }

    #[test]
    fn run_directory_three_states_match_java_semantics() {
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test-instance-settings")
            .join(format!("{:x}", std::process::id()));
        let repo = GameRepository::new(&dir);

        let not_overridden = InstanceGameSettings::default();
        assert_eq!(
            not_overridden.run_directory(&repo, "1.20.1-forge", false),
            repo.root
        );

        let mut isolated = InstanceGameSettings {
            running_directory: Some(String::new()),
            ..Default::default()
        };
        isolated.set_overridden(PROPERTY_RUNNING_DIRECTORY);
        assert_eq!(
            isolated.run_directory(&repo, "1.20.1-forge", false),
            repo.version_root("1.20.1-forge")
        );

        let mut custom = InstanceGameSettings {
            running_directory: Some("D:/custom/run/dir".to_string()),
            ..Default::default()
        };
        custom.set_overridden(PROPERTY_RUNNING_DIRECTORY);
        assert_eq!(
            custom.run_directory(&repo, "1.20.1-forge", false),
            std::path::PathBuf::from("D:/custom/run/dir")
        );

        let modpack_ignores_override = not_overridden.run_directory(&repo, "some-modpack", true);
        assert_eq!(modpack_ignores_override, repo.version_root("some-modpack"));
    }

    #[test]
    fn round_trips_through_json_preserving_unknown_fields() {
        let json = serde_json::json!({
            "$schema": "https://schemas.glavo.site/hmcl/instance-game-settings/1.0.0",
            "overrideProperties": ["maxMemory", "windowType"],
            "maxMemory": 4096,
            "windowType": "FULLSCREEN",
            "launcherVisibility": "KEEP",
            "processPriority": "HIGH",
            "showLogs": true
        });
        let settings: InstanceGameSettings = serde_json::from_value(json).unwrap();
        assert_eq!(settings.max_memory, Some(4096));
        assert_eq!(settings.window_type, Some(WindowType::Fullscreen));
        assert!(settings.is_overridden(PROPERTY_MAX_MEMORY));
        assert_eq!(settings.launcher_visibility, Some(LauncherVisibility::Keep));
        assert_eq!(settings.process_priority, Some(ProcessPriority::High));
        assert_eq!(
            settings.extra.get("showLogs").and_then(|v| v.as_bool()),
            Some(true)
        );

        let back = serde_json::to_value(&settings).unwrap();
        assert_eq!(
            back["showLogs"], true,
            "unmodeled fields must survive a round trip"
        );
        assert_eq!(back["launcherVisibility"], "KEEP");
        assert_eq!(back["processPriority"], "HIGH");
    }

    #[test]
    fn new_effective_methods_fall_back_to_hardcoded_defaults_when_not_overridden() {
        let settings = InstanceGameSettings::default();
        assert_eq!(settings.effective_min_memory(), None);
        assert_eq!(settings.effective_metaspace(), None);
        assert_eq!(settings.effective_window(854, 480), (854, 480, false));
        assert_eq!(settings.effective_quick_play_option(), None);
        assert_eq!(settings.effective_game_arguments(), None);
        assert_eq!(settings.effective_wrapper(), None);
        assert_eq!(
            settings.effective_process_priority(),
            crate::launch::ProcessPriority::Normal
        );
        assert!(!settings.effective_debug_log_output());
        assert!(!settings.effective_use_custom_natives());
        assert_eq!(settings.effective_natives_directory(), None);
        assert_eq!(settings.effective_environment_variables(), Vec::new());
        assert_eq!(settings.effective_pre_launch_command(), None);
        assert_eq!(settings.effective_post_exit_command(), None);
        assert_eq!(
            settings.effective_launcher_visibility(),
            LauncherVisibility::Keep
        );
    }

    #[test]
    fn window_maximized_degrades_to_windowed_resolution_not_fullscreen() {
        let mut settings = InstanceGameSettings {
            window_type: Some(WindowType::Maximized),
            width: Some(1920.0),
            height: Some(1080.0),
            ..Default::default()
        };
        settings.set_overridden(PROPERTY_WINDOW_TYPE);
        settings.set_overridden(PROPERTY_WIDTH);
        settings.set_overridden(PROPERTY_HEIGHT);
        let (width, height, fullscreen) = settings.effective_window(854, 480);
        assert_eq!((width, height), (1920, 1080));
        assert!(!fullscreen, "Maximized must not be treated as fullscreen");
    }

    #[test]
    fn quick_play_option_resolves_the_right_variant_and_ignores_empty_values() {
        let mut settings = InstanceGameSettings {
            quick_play: Some(QuickPlayType::Singleplayer),
            quick_play_singleplayer: Some("My World".to_string()),
            ..Default::default()
        };
        settings.set_overridden(PROPERTY_QUICK_PLAY);
        assert_eq!(
            settings.effective_quick_play_option(),
            Some(crate::launch::QuickPlayOption::SinglePlayer {
                world_folder_name: "My World".to_string()
            })
        );

        let mut empty = InstanceGameSettings {
            quick_play: Some(QuickPlayType::Singleplayer),
            quick_play_singleplayer: Some(String::new()),
            ..Default::default()
        };
        empty.set_overridden(PROPERTY_QUICK_PLAY);
        assert_eq!(empty.effective_quick_play_option(), None);

        let mut realms = InstanceGameSettings {
            quick_play: Some(QuickPlayType::Realms),
            quick_play_realms: Some("abc123".to_string()),
            ..Default::default()
        };
        realms.set_overridden(PROPERTY_QUICK_PLAY);
        assert_eq!(
            realms.effective_quick_play_option(),
            Some(crate::launch::QuickPlayOption::Realm {
                realm_id: "abc123".to_string()
            })
        );
    }

    #[test]
    fn environment_variables_parse_one_key_value_per_line_and_skip_blank_lines() {
        let mut settings = InstanceGameSettings {
            environment_variables: Some(
                "FOO=bar\n\nBAZ = qux with spaces\nnotakeyvalueline".to_string(),
            ),
            ..Default::default()
        };
        settings.set_overridden(PROPERTY_ENVIRONMENT_VARIABLES);
        assert_eq!(
            settings.effective_environment_variables(),
            vec![
                ("FOO".to_string(), "bar".to_string()),
                ("BAZ".to_string(), "qux with spaces".to_string())
            ]
        );
    }

    #[test]
    fn process_priority_and_launcher_visibility_serde_match_java_screaming_snake_case() {
        let settings = InstanceGameSettings {
            process_priority: Some(ProcessPriority::BelowNormal),
            launcher_visibility: Some(LauncherVisibility::Minimize),
            ..Default::default()
        };
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["processPriority"], "BELOW_NORMAL");
        assert_eq!(json["launcherVisibility"], "MINIMIZE");
        let legacy: InstanceGameSettings =
            serde_json::from_value(serde_json::json!({ "launcherVisibility": "HIDE" })).unwrap();
        assert_eq!(
            legacy.launcher_visibility,
            Some(LauncherVisibility::Minimize)
        );
        let legacy: InstanceGameSettings =
            serde_json::from_value(serde_json::json!({ "launcherVisibility": "HIDE_AND_REOPEN" }))
                .unwrap();
        assert_eq!(
            legacy.launcher_visibility,
            Some(LauncherVisibility::Minimize)
        );
    }

    #[test]
    fn graphics_backend_uses_java_spelling_and_only_applies_when_overridden() {
        let mut settings = InstanceGameSettings {
            graphics_backend: Some(crate::launch::GraphicsApi::OpenGl),
            ..Default::default()
        };
        assert_eq!(
            settings.effective_graphics_backend(),
            crate::launch::GraphicsApi::Default
        );
        settings.set_overridden(PROPERTY_GRAPHICS_BACKEND);
        assert_eq!(
            settings.effective_graphics_backend(),
            crate::launch::GraphicsApi::OpenGl
        );
        assert_eq!(
            serde_json::to_value(&settings).unwrap()["graphicsBackend"],
            "OPENGL"
        );
    }

    #[test]
    fn instance_settings_path_matches_java_layout() {
        let repo = GameRepository::new("C:/mc");
        let path = instance_settings_path(&repo, "1.20.1-forge");
        let comps: Vec<_> = path
            .components()
            .rev()
            .take(4)
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            comps,
            vec![
                "instance-game-settings.json",
                "config",
                ".hmcl",
                "1.20.1-forge"
            ]
        );
    }
}
