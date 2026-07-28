use serde::{Deserialize, Serialize};

use super::TypedId;

pub const SCHEMA_ID: &str = "game-directories";
const GAME_DIRECTORY_ID_PREFIX: &str = "game-directory";

/// 对应 Java `GameDirectoryManager` 里两个写死的内置目录 ID——首次运行时如果对应
/// 文件是空的就会自动创建这两条。照抄这两个 UUID 字面量，不能自己另生成一个，
/// 否则跟现有 HMCL 安装的默认目录对不上号（用户会看到重复的两条"默认目录"）。
pub const LOCAL_DEFAULT_ID: &str = "game-directory:7105bc1f-490e-5e8c-878c-f5844c3d4bc3";
pub const USER_DEFAULT_ID: &str = "game-directory:f3eafde8-506e-5a77-bc88-f24b4728dfb2";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GameDirectoriesFile {
    #[serde(default)]
    pub directories: Vec<GameDirectory>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameDirectory {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<LocalizedText>,
    pub path: String,
    #[serde(
        default,
        rename = "legacyGameSettings",
        skip_serializing_if = "Option::is_none"
    )]
    pub legacy_game_settings: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl GameDirectory {
    pub fn new(name: Option<String>, path: impl Into<String>) -> GameDirectory {
        GameDirectory {
            id: TypedId::generate(GAME_DIRECTORY_ID_PREFIX).to_string(),
            name: name.map(LocalizedText::Plain),
            path: path.into(),
            legacy_game_settings: None,
            extra: Default::default(),
        }
    }

    pub fn is_path_absolute(&self) -> bool {
        is_portable_path_absolute(&self.path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LocalizedText {
    Plain(String),
    ByLocale(std::collections::HashMap<String, String>),
}

/// 对应 Java `PortablePath.isAbsolute()`：以 `/`、`\` 开头，或者是
/// Windows 盘符（单个字母 + `:`）就算绝对路径。这不是存在 JSON 里的独立字段，
/// 是每次读的时候用同一套规则重新判断的，所以这个函数的行为必须和 Java 位对位
/// 一致，不能自己"优化"成用 `Path::is_absolute()`（那个在 Unix 语义下对
/// `C:\xxx` 这种 Windows 路径字符串的判断是错的）。
pub fn is_portable_path_absolute(s: &str) -> bool {
    if s.starts_with('/') || s.starts_with('\\') {
        return true;
    }
    let bytes = s.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

pub fn normalize_portable_path(s: &str) -> String {
    if is_portable_path_absolute(s) {
        s.to_string()
    } else {
        s.replace('\\', "/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_path_absoluteness_matches_java_heuristic() {
        assert!(is_portable_path_absolute("/home/user/.minecraft"));
        assert!(is_portable_path_absolute(r"\\server\share"));
        assert!(is_portable_path_absolute(r"C:\Users\Steve\.minecraft"));
        assert!(is_portable_path_absolute("D:/games/minecraft"));
        assert!(!is_portable_path_absolute(".minecraft"));
        assert!(!is_portable_path_absolute("versions/1.20.1"));
    }

    #[test]
    fn relative_paths_get_backslashes_normalized_absolute_paths_do_not() {
        assert_eq!(normalize_portable_path(r"mods\1.20.1"), "mods/1.20.1");
        assert_eq!(
            normalize_portable_path(r"C:\Users\Steve\.minecraft"),
            r"C:\Users\Steve\.minecraft"
        );
    }

    #[test]
    fn round_trips_a_directory_list_with_localized_name() {
        let mut file = GameDirectoriesFile::default();
        file.directories.push(GameDirectory::new(
            Some("My Modpack".to_string()),
            ".minecraft",
        ));

        let json = serde_json::to_value(&file).unwrap();
        let back: GameDirectoriesFile = serde_json::from_value(json).unwrap();
        assert_eq!(back.directories.len(), 1);
        assert!(
            matches!(back.directories[0].name, Some(LocalizedText::Plain(ref s)) if s == "My Modpack")
        );
        assert!(!back.directories[0].is_path_absolute());
    }

    #[test]
    fn known_default_directory_ids_match_hmcl_exactly() {
        assert_eq!(
            LOCAL_DEFAULT_ID,
            "game-directory:7105bc1f-490e-5e8c-878c-f5844c3d4bc3"
        );
        assert_eq!(
            USER_DEFAULT_ID,
            "game-directory:f3eafde8-506e-5a77-bc88-f24b4728dfb2"
        );
    }
}
