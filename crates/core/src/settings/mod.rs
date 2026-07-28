pub mod accounts;
pub mod authlib_injector_servers;
pub mod game_directories;
pub mod instance_game_settings;
pub mod launcher_settings;
pub mod typed_id;

pub use typed_id::TypedId;

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const SCHEMA_HOST: &str = "https://schemas.glavo.site/hmcl";

static LAUNCHER_DATA_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let legacy = PathBuf::from(".hmcl");
    let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) else {
        return legacy;
    };
    let target = roaming.join(".hmcl-rs");
    if target.exists() || !legacy.is_dir() {
        return target;
    }

    let staging = roaming.join(format!(".hmcl-rs.migrating-{}", std::process::id()));
    if copy_directory(&legacy, &staging)
        .and_then(|_| std::fs::rename(&staging, &target))
        .is_err()
    {
        let _ = std::fs::remove_dir_all(staging);
    }
    target.exists().then_some(target).unwrap_or(legacy)
});

/// 启动器自身的数据目录。Windows 使用 `%APPDATA%\.hmcl-rs`；旧版的本地
/// `.hmcl` 会先完整复制过去，原目录不会被移动或删除。
pub fn launcher_data_dir() -> PathBuf {
    LAUNCHER_DATA_DIR.clone()
}

fn copy_directory(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_directory(&entry.path(), &destination)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), destination)?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "launcher data contains an unsupported filesystem entry",
            ));
        }
    }
    Ok(())
}

pub fn schema_url(id: &str) -> String {
    format!("{SCHEMA_HOST}/{id}/1.0.0")
}

fn schema_matches(json: &serde_json::Value, expected_id: &str) -> bool {
    let Some(schema) = json.get("$schema").and_then(|v| v.as_str()) else {
        return false;
    };
    schema.starts_with(&format!("{SCHEMA_HOST}/{expected_id}/1."))
}

#[derive(Debug)]
pub struct Loaded<T> {
    pub value: T,
    /// `false` 表示这份配置目前是默认值——要么文件不存在（这种反而 `can_save=true`，
    /// 因为写一个全新文件没有丢东西的风险），要么文件存在但 `$schema`
    /// 认不出来/主版本号不支持（这种 `can_save=false`：我们读不懂不代表这文件
    /// 是坏的，可能是更高版本的 HMCL 写的，贸然保存会用我们的默认值覆盖掉）。
    pub can_save: bool,
}

fn backup_invalid_config(path: &Path) {
    if !path.is_file() {
        return;
    }
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    for i in 1..1000 {
        let candidate = path.with_file_name(format!("{file_name}.{i}"));
        if !candidate.exists() {
            let _ = std::fs::rename(path, candidate);
            return;
        }
    }
}

pub fn load<T>(path: &Path, expected_id: &str) -> Loaded<T>
where
    T: serde::de::DeserializeOwned + Default,
{
    let Ok(text) = std::fs::read_to_string(path) else {
        return Loaded {
            value: T::default(),
            can_save: true,
        };
    };

    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&text) else {
        tracing::warn!(path = %path.display(), "settings file is not valid JSON, backing it up and using defaults");
        backup_invalid_config(path);
        return Loaded {
            value: T::default(),
            can_save: true,
        };
    };

    if !schema_matches(&json, expected_id) {
        tracing::warn!(path = %path.display(), expected_id, "unrecognized or incompatible $schema, using defaults without touching the file");
        return Loaded {
            value: T::default(),
            can_save: false,
        };
    }

    if let Some(obj) = json.as_object_mut() {
        obj.remove("$schema");
    }

    match serde_json::from_value(json) {
        Ok(value) => Loaded {
            value,
            can_save: true,
        },
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "recognized schema but failed to parse contents, backing up and using defaults");
            backup_invalid_config(path);
            Loaded {
                value: T::default(),
                can_save: true,
            }
        }
    }
}

pub fn save<T>(path: &Path, expected_id: &str, value: &T) -> std::io::Result<()>
where
    T: serde::Serialize,
{
    let mut json = serde_json::to_value(value).expect("settings types must always serialize");
    if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "$schema".to_string(),
            serde_json::Value::String(schema_url(expected_id)),
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(&json)?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
    struct Dummy {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        foo: Option<String>,
        #[serde(flatten)]
        extra: serde_json::Map<String, serde_json::Value>,
    }

    fn tmp_file(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test-settings")
            .join(format!("{:x}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn launcher_data_migration_copies_without_removing_the_source() {
        let root = tmp_file("migration");
        let source = root.join("source");
        let target = root.join("target");
        let nested = source.join("config");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("settings.json"), b"settings").unwrap();

        copy_directory(&source, &target).unwrap();

        assert_eq!(
            std::fs::read(target.join("config/settings.json")).unwrap(),
            b"settings"
        );
        assert_eq!(
            std::fs::read(source.join("config/settings.json")).unwrap(),
            b"settings"
        );
    }

    #[test]
    fn missing_file_uses_default_and_is_savable() {
        let path = tmp_file("missing.json");
        let loaded: Loaded<Dummy> = load(&path, "dummy");
        assert_eq!(loaded.value, Dummy::default());
        assert!(loaded.can_save);
    }

    #[test]
    fn round_trips_through_save_and_load() {
        let path = tmp_file("roundtrip.json");
        let value = Dummy {
            foo: Some("bar".to_string()),
            extra: Default::default(),
        };
        save(&path, "dummy", &value).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("https://schemas.glavo.site/hmcl/dummy/1.0.0"));

        let loaded: Loaded<Dummy> = load(&path, "dummy");
        assert_eq!(loaded.value, value);
        assert!(loaded.can_save);
    }

    #[test]
    fn unknown_fields_survive_a_round_trip_via_flatten() {
        let path = tmp_file("unknown_fields.json");
        std::fs::write(
            &path,
            r#"{"$schema": "https://schemas.glavo.site/hmcl/dummy/1.0.0", "foo": "bar", "somethingFromANewerHmcl": 42}"#,
        )
        .unwrap();

        let loaded: Loaded<Dummy> = load(&path, "dummy");
        assert!(loaded.can_save);
        assert_eq!(loaded.value.foo.as_deref(), Some("bar"));
        assert_eq!(
            loaded
                .value
                .extra
                .get("somethingFromANewerHmcl")
                .and_then(|v| v.as_i64()),
            Some(42)
        );

        save(&path, "dummy", &loaded.value).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("somethingFromANewerHmcl"),
            "unknown field must survive load -> save round trip: {text}"
        );
    }

    #[test]
    fn wrong_schema_id_is_treated_as_unreadable_and_not_savable() {
        let path = tmp_file("wrong_id.json");
        std::fs::write(&path, r#"{"$schema": "https://schemas.glavo.site/hmcl/game-settings/1.0.0", "foo": "should not be read as dummy"}"#).unwrap();

        let loaded: Loaded<Dummy> = load(&path, "dummy");
        assert_eq!(loaded.value, Dummy::default());
        assert!(
            !loaded.can_save,
            "must refuse to overwrite a file belonging to a different schema"
        );
    }

    #[test]
    fn unsupported_major_version_is_treated_as_unreadable_and_not_savable() {
        let path = tmp_file("future_major.json");
        std::fs::write(&path, r#"{"$schema": "https://schemas.glavo.site/hmcl/dummy/2.0.0", "foo": "from the future"}"#).unwrap();

        let loaded: Loaded<Dummy> = load(&path, "dummy");
        assert_eq!(loaded.value, Dummy::default());
        assert!(
            !loaded.can_save,
            "a future major schema version must not be silently downgraded/overwritten"
        );
    }

    #[test]
    fn malformed_json_is_backed_up_before_defaults_are_used() {
        let path = tmp_file("malformed.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let loaded: Loaded<Dummy> = load(&path, "dummy");
        assert_eq!(loaded.value, Dummy::default());
        assert!(loaded.can_save);
        assert!(
            !path.exists(),
            "the malformed original should have been moved aside"
        );
        assert!(
            path.with_file_name("malformed.json.1").exists(),
            "backup copy must exist"
        );
    }
}
