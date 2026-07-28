use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum DataPackError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to read zip archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("{0} 不是一个数据包（缺少 pack.mcmeta）")]
    NotADataPack(String),
    #[error("{0} 已存在")]
    AlreadyExists(String),
}

const DISABLED_SUFFIX: &str = ".disabled";

#[derive(Debug, Clone)]
pub struct DataPack {
    pub path: PathBuf,
    /// 展示名：去掉 `.zip`/`.disabled` 之后的文件名。
    pub id: String,
    pub description: String,
    pub enabled: bool,
    pub is_directory: bool,
}

#[derive(Debug, Deserialize)]
struct PackMcMeta {
    pack: PackSection,
}

#[derive(Debug, Deserialize)]
struct PackSection {
    #[serde(default)]
    description: serde_json::Value,
}

fn flatten_description(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => items.iter().map(flatten_description).collect(),
        serde_json::Value::Object(map) => {
            let mut text = map.get("text").map(flatten_description).unwrap_or_default();
            if let Some(extra) = map.get("extra") {
                text.push_str(&flatten_description(extra));
            }
            text
        }
        _ => String::new(),
    }
}

fn parse_description(raw: &str) -> String {
    serde_json::from_str::<PackMcMeta>(raw)
        .map(|meta| flatten_description(&meta.pack.description))
        .unwrap_or_default()
}

fn display_id(file_name: &str) -> String {
    let name = file_name.strip_suffix(DISABLED_SUFFIX).unwrap_or(file_name);
    name.strip_suffix(".zip").unwrap_or(name).to_string()
}

pub fn directory_of(world_path: &Path) -> PathBuf {
    world_path.join("datapacks")
}

pub fn list(datapacks_dir: &Path) -> Vec<DataPack> {
    let Ok(entries) = std::fs::read_dir(datapacks_dir) else {
        return Vec::new();
    };
    let mut packs: Vec<DataPack> = entries
        .flatten()
        .filter_map(|entry| read_pack(&entry.path()))
        .collect();
    packs.sort_by_key(|pack| pack.id.to_lowercase());
    packs
}

fn read_pack(path: &Path) -> Option<DataPack> {
    let file_name = path.file_name()?.to_str()?.to_string();
    if path.is_dir() {
        let enabled_meta = path.join("pack.mcmeta");
        let disabled_meta = path.join("pack.mcmeta.disabled");
        let (meta, enabled) = if enabled_meta.is_file() {
            (enabled_meta, true)
        } else if disabled_meta.is_file() {
            (disabled_meta, false)
        } else {
            return None;
        };
        Some(DataPack {
            description: parse_description(&std::fs::read_to_string(meta).ok()?),
            id: display_id(&file_name),
            path: path.to_path_buf(),
            enabled,
            is_directory: true,
        })
    } else {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
        let mut raw = String::new();
        archive
            .by_name("pack.mcmeta")
            .ok()?
            .read_to_string(&mut raw)
            .ok()?;
        Some(DataPack {
            description: parse_description(&raw),
            id: display_id(&file_name),
            path: path.to_path_buf(),
            enabled: !file_name.ends_with(DISABLED_SUFFIX),
            is_directory: false,
        })
    }
}

pub fn set_enabled(pack: &DataPack, enabled: bool) -> Result<PathBuf, DataPackError> {
    if pack.enabled == enabled {
        return Ok(pack.path.clone());
    }
    let target = if pack.is_directory {
        pack.path.join("pack.mcmeta")
    } else {
        pack.path.clone()
    };
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| DataPackError::NotADataPack(pack.id.clone()))?;
    let renamed = if enabled {
        file_name
            .strip_suffix(DISABLED_SUFFIX)
            .unwrap_or(file_name)
            .to_string()
    } else {
        format!("{file_name}{DISABLED_SUFFIX}")
    };
    let from = if enabled {
        target.with_file_name(format!("{file_name}{DISABLED_SUFFIX}"))
    } else {
        target.clone()
    };
    let from = if from.exists() { from } else { target.clone() };
    let to = from.with_file_name(renamed);
    if from != to {
        std::fs::rename(&from, &to)?;
    }
    Ok(if pack.is_directory {
        pack.path.clone()
    } else {
        to
    })
}

pub fn delete(pack: &DataPack) -> Result<(), DataPackError> {
    if pack.is_directory {
        std::fs::remove_dir_all(&pack.path)?;
    } else {
        std::fs::remove_file(&pack.path)?;
    }
    Ok(())
}

pub fn install(datapacks_dir: &Path, source: &Path) -> Result<PathBuf, DataPackError> {
    let file_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| DataPackError::NotADataPack(source.display().to_string()))?;

    let mut archive = zip::ZipArchive::new(std::fs::File::open(source)?)?;
    if archive.by_name("pack.mcmeta").is_err() {
        return Err(DataPackError::NotADataPack(file_name.to_string()));
    }

    std::fs::create_dir_all(datapacks_dir)?;
    let target = datapacks_dir.join(file_name);
    if target.exists() {
        return Err(DataPackError::AlreadyExists(file_name.to_string()));
    }
    std::fs::copy(source, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hmcl-rs-datapack-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_zip_pack(path: &Path, description: &str) {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let options: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        writer.start_file("pack.mcmeta", options).unwrap();
        std::io::Write::write_all(
            &mut writer,
            format!(r#"{{"pack":{{"pack_format":15,"description":{description}}}}}"#).as_bytes(),
        )
        .unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn reads_zip_and_directory_packs_with_their_enabled_state() {
        let dir = temp_dir("list");
        write_zip_pack(&dir.join("bundled.zip"), r#""一个数据包""#);
        write_zip_pack(&dir.join("off.zip.disabled"), r#""关掉的""#);

        let folder = dir.join("unpacked");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("pack.mcmeta.disabled"),
            r#"{"pack":{"pack_format":15,"description":"目录形态"}}"#,
        )
        .unwrap();

        let packs = list(&dir);
        assert_eq!(packs.len(), 3);
        let by_id = |id: &str| packs.iter().find(|p| p.id == id).unwrap();
        assert!(by_id("bundled").enabled);
        assert_eq!(by_id("bundled").description, "一个数据包");
        assert!(!by_id("off").enabled);
        assert!(!by_id("unpacked").enabled);
        assert!(by_id("unpacked").is_directory);
    }

    #[test]
    fn toggling_renames_the_zip_and_the_directorys_mcmeta() {
        let dir = temp_dir("toggle");
        write_zip_pack(&dir.join("pack.zip"), r#""x""#);
        let folder = dir.join("folder");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("pack.mcmeta"),
            r#"{"pack":{"description":"y"}}"#,
        )
        .unwrap();

        let packs = list(&dir);
        for pack in &packs {
            set_enabled(pack, false).unwrap();
        }
        assert!(dir.join("pack.zip.disabled").is_file());
        assert!(folder.join("pack.mcmeta.disabled").is_file());
        assert!(list(&dir).iter().all(|pack| !pack.enabled));

        for pack in &list(&dir) {
            set_enabled(pack, true).unwrap();
        }
        assert!(dir.join("pack.zip").is_file());
        assert!(folder.join("pack.mcmeta").is_file());
        assert!(list(&dir).iter().all(|pack| pack.enabled));
    }

    #[test]
    fn description_accepts_rich_text_components() {
        assert_eq!(
            parse_description(r#"{"pack":{"description":[{"text":"甲"},{"text":"乙"}]}}"#),
            "甲乙"
        );
        assert_eq!(
            parse_description(r#"{"pack":{"description":{"text":"甲","extra":["乙"]}}}"#),
            "甲乙"
        );
    }

    #[test]
    fn install_rejects_a_zip_without_pack_mcmeta() {
        let dir = temp_dir("install");
        let bogus = dir.join("bogus.zip");
        zip::ZipWriter::new(std::fs::File::create(&bogus).unwrap())
            .finish()
            .unwrap();
        assert!(matches!(
            install(&dir.join("datapacks"), &bogus),
            Err(DataPackError::NotADataPack(_))
        ));

        let good = dir.join("good.zip");
        write_zip_pack(&good, r#""ok""#);
        let installed = install(&dir.join("datapacks"), &good).unwrap();
        assert!(installed.is_file());
        assert!(matches!(
            install(&dir.join("datapacks"), &good),
            Err(DataPackError::AlreadyExists(_))
        ));
    }
}
