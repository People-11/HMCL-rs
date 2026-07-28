use serde::{Deserialize, Serialize};

pub const SCHEMA_ID: &str = "launcher-settings";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LauncherSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(
        default,
        rename = "titleBarTransparent",
        skip_serializing_if = "Option::is_none"
    )]
    pub title_bar_transparent: Option<bool>,
    #[serde(
        default,
        rename = "animationDisabled",
        skip_serializing_if = "Option::is_none"
    )]
    pub animation_disabled: Option<bool>,

    #[serde(
        default,
        rename = "downloadThreads",
        skip_serializing_if = "Option::is_none"
    )]
    pub download_threads: Option<u32>,
    #[serde(
        default,
        rename = "autoDownloadThreads",
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_download_threads: Option<bool>,
    #[serde(
        default,
        rename = "versionListSource",
        skip_serializing_if = "Option::is_none"
    )]
    pub version_list_source: Option<DownloadSource>,
    #[serde(
        default,
        rename = "fileDownloadSource",
        skip_serializing_if = "Option::is_none"
    )]
    pub file_download_source: Option<DownloadSource>,

    #[serde(
        default,
        rename = "hasProxyAuth",
        skip_serializing_if = "Option::is_none"
    )]
    pub has_proxy_auth: Option<bool>,
    #[serde(default, rename = "proxyType", skip_serializing_if = "Option::is_none")]
    pub proxy_type: Option<ProxyType>,
    #[serde(default, rename = "proxyHost", skip_serializing_if = "Option::is_none")]
    pub proxy_host: Option<String>,
    #[serde(default, rename = "proxyPort", skip_serializing_if = "Option::is_none")]
    pub proxy_port: Option<u16>,
    #[serde(default, rename = "proxyUser", skip_serializing_if = "Option::is_none")]
    pub proxy_user: Option<String>,
    #[serde(
        default,
        rename = "proxyPassword",
        skip_serializing_if = "Option::is_none"
    )]
    pub proxy_password: Option<String>,

    #[serde(
        default,
        rename = "selectedGameDirectory",
        skip_serializing_if = "Option::is_none"
    )]
    pub selected_game_directory: Option<String>,
    #[serde(
        default,
        rename = "defaultGameSettingsPreset",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_game_settings_preset: Option<String>,
    #[serde(
        default,
        rename = "selectedInstance",
        skip_serializing_if = "std::collections::HashMap::is_empty"
    )]
    pub selected_instance: std::collections::HashMap<String, String>,
    #[serde(
        default,
        rename = "selectedAccount",
        skip_serializing_if = "Option::is_none"
    )]
    pub selected_account: Option<String>,

    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadSource {
    #[serde(rename = "DEFAULT")]
    Default,
    #[serde(rename = "OFFICIAL")]
    Official,
    #[serde(rename = "MIRROR")]
    Mirror,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyType {
    #[serde(rename = "SYSTEM")]
    System,
    #[serde(rename = "DIRECT")]
    Direct,
    #[serde(rename = "HTTP")]
    Http,
    #[serde(rename = "SOCKS")]
    Socks,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmodeled_ui_fields_survive_round_trip() {
        let json = serde_json::json!({
            "language": "zh_CN",
            "downloadThreads": 32,
            "themeColorStyle": "VIBRANT",
            "customBackgroundImagePath": "C:/wallpapers/bg.png",
            "windowTransparent": true
        });
        let settings: LauncherSettings = serde_json::from_value(json).unwrap();
        assert_eq!(settings.language.as_deref(), Some("zh_CN"));
        assert_eq!(settings.download_threads, Some(32));
        assert_eq!(
            settings
                .extra
                .get("themeColorStyle")
                .and_then(|v| v.as_str()),
            Some("VIBRANT")
        );

        let back = serde_json::to_value(&settings).unwrap();
        assert_eq!(back["customBackgroundImagePath"], "C:/wallpapers/bg.png");
        assert_eq!(back["windowTransparent"], true);
    }

    #[test]
    fn download_source_enum_matches_schema_casing() {
        let settings = LauncherSettings {
            version_list_source: Some(DownloadSource::Mirror),
            ..Default::default()
        };
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["versionListSource"], "MIRROR");
    }
}
