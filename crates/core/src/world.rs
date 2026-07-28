use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fastnbt::Value;

use crate::versioning::GameVersionNumber;

#[derive(Debug, thiserror::Error)]
pub enum WorldError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to read zip archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("failed to parse NBT: {0}")]
    Nbt(#[from] fastnbt::error::Error),
    #[error("{0} is not a Minecraft world")]
    NotAWorld(String),
    #[error("world {0} is in use by a running game")]
    Locked(String),
    #[error("{0} already exists")]
    AlreadyExists(String),
    #[error("invalid world name {0:?}")]
    InvalidName(String),
    #[error("this world was read from a zip archive and cannot be modified in place")]
    ReadOnly,
}

const LEVEL_FILE_NAMES: [&str; 2] = ["level.dat", "special_level.dat"];

const MIN_DATA_PACK_VERSION: &str = "1.13";
const MIN_QUICK_PLAY_VERSION: &str = "1.20";

fn tag<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Compound(map) => map.get(key),
        _ => None,
    }
}

fn tag_mut<'a>(value: &'a mut Value, key: &str) -> Option<&'a mut Value> {
    match value {
        Value::Compound(map) => map.get_mut(key),
        _ => None,
    }
}

fn as_str(value: Option<&Value>) -> Option<&str> {
    match value? {
        Value::String(s) => Some(s),
        _ => None,
    }
}

fn as_long(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Long(v) => Some(*v),
        _ => None,
    }
}

fn as_int(value: Option<&Value>) -> Option<i32> {
    match value? {
        Value::Int(v) => Some(*v),
        _ => None,
    }
}

fn as_float(value: Option<&Value>) -> Option<f32> {
    match value? {
        Value::Float(v) => Some(*v),
        _ => None,
    }
}

fn as_bool(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Byte(0) => Some(false),
        Value::Byte(1) => Some(true),
        _ => None,
    }
}

/// 只在原标签**已经是目标类型**时赋值，返回是否改动成功。这是"绝不改变标签类型"
/// 那条硬约束在代码里的落点：字段不存在或类型对不上就什么都不做，让 UI 把控件
/// 禁用掉，而不是凭空造一个新类型的标签写进存档。
fn set_byte(container: &mut Value, key: &str, value: bool) -> bool {
    match tag_mut(container, key) {
        Some(slot @ Value::Byte(_)) => {
            *slot = Value::Byte(i8::from(value));
            true
        }
        _ => false,
    }
}

fn set_int(container: &mut Value, key: &str, value: i32) -> bool {
    match tag_mut(container, key) {
        Some(slot @ Value::Int(_)) => {
            *slot = Value::Int(value);
            true
        }
        _ => false,
    }
}

fn set_float(container: &mut Value, key: &str, value: f32) -> bool {
    match tag_mut(container, key) {
        Some(slot @ Value::Float(_)) => {
            *slot = Value::Float(value);
            true
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    pub dimension: Option<String>,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

fn dimension_name(value: Option<&Value>) -> Option<Option<String>> {
    match value? {
        // 1.16 之前维度是数字 id。
        Value::Int(0) => Some(None),
        Value::Int(-1) => Some(Some("下界".into())),
        Value::Int(1) => Some(Some("末地".into())),
        Value::Int(_) => None,
        Value::String(id) => Some(match id.as_str() {
            "overworld" | "minecraft:overworld" => None,
            "the_nether" | "minecraft:the_nether" => Some("下界".into()),
            "the_end" | "minecraft:the_end" => Some("末地".into()),
            other => Some(other.to_string()),
        }),
        _ => None,
    }
}

fn coordinates(value: Option<&Value>) -> Option<(f64, f64, f64)> {
    match value? {
        Value::List(items) => match items.as_slice() {
            [Value::Double(x), Value::Double(y), Value::Double(z)] => Some((*x, *y, *z)),
            _ => None,
        },
        Value::IntArray(array) => match array.as_ref() {
            [x, y, z] => Some((*x as f64, *y as f64, *z as f64)),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Difficulty {
    Peaceful,
    Easy,
    Normal,
    Hard,
}

impl Difficulty {
    pub const ALL: [Difficulty; 4] = [
        Difficulty::Peaceful,
        Difficulty::Easy,
        Difficulty::Normal,
        Difficulty::Hard,
    ];

    pub fn index(self) -> usize {
        Difficulty::ALL.iter().position(|d| *d == self).unwrap()
    }

    pub fn from_index(index: usize) -> Option<Difficulty> {
        Difficulty::ALL.get(index).copied()
    }

    fn id(self) -> &'static str {
        match self {
            Difficulty::Peaceful => "peaceful",
            Difficulty::Easy => "easy",
            Difficulty::Normal => "normal",
            Difficulty::Hard => "hard",
        }
    }

    fn from_id(id: &str) -> Option<Difficulty> {
        Difficulty::ALL.into_iter().find(|d| d.id() == id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameType {
    Survival,
    Creative,
    Adventure,
    Spectator,
    Hardcore,
}

impl GameType {
    pub const ALL: [GameType; 5] = [
        GameType::Survival,
        GameType::Creative,
        GameType::Adventure,
        GameType::Spectator,
        GameType::Hardcore,
    ];

    pub fn index(self) -> usize {
        GameType::ALL.iter().position(|g| *g == self).unwrap()
    }

    pub fn from_index(index: usize) -> Option<GameType> {
        GameType::ALL.get(index).copied()
    }
}

/// `level.dat` 正常是 gzip 的，但历史上也出现过未压缩和 zlib 压缩的存档，
/// 而且备份/整合包里的文件谁都可能重新打包过。按魔数分派比"假定 gzip"稳。
fn read_nbt(bytes: &[u8]) -> Result<Value, WorldError> {
    let mut decoded = Vec::new();
    let slice = if bytes.starts_with(&[0x1f, 0x8b]) {
        flate2::read::GzDecoder::new(bytes).read_to_end(&mut decoded)?;
        &decoded[..]
    } else if bytes.first() == Some(&0x78) {
        flate2::read::ZlibDecoder::new(bytes).read_to_end(&mut decoded)?;
        &decoded[..]
    } else {
        bytes
    };
    Ok(fastnbt::from_bytes(slice)?)
}

fn write_nbt(path: &Path, value: &Value) -> Result<(), WorldError> {
    let raw = fastnbt::to_bytes(value)?;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&raw)?;
    let compressed = encoder.finish()?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| WorldError::NotAWorld(path.display().to_string()))?;
    let tmp = path.with_file_name(format!("{file_name}.hmcl-tmp"));
    std::fs::write(&tmp, &compressed)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub struct World {
    path: PathBuf,
    zip_root: Option<String>,
    file_name: String,
    level: Value,
    level_file_name: &'static str,
    /// 外置的 `data/minecraft/world_gen_settings.dat`（新版本把 WorldGenSettings
    /// 从 level.dat 里挪出去了）。存的是**整个文件的根标签**，真正要读的内容在
    /// 它的 `data` 子标签里——写回时必须写根标签，见 [`World::world_gen_settings`]。
    world_gen_settings: Option<(PathBuf, Value)>,
    player: Option<(PathBuf, Value)>,
}

impl World {
    pub fn open(path: &Path) -> Result<World, WorldError> {
        if path.is_dir() {
            World::open_directory(path)
        } else if path.is_file() {
            World::open_zip(path)
        } else {
            Err(WorldError::NotAWorld(path.display().to_string()))
        }
    }

    fn open_directory(path: &Path) -> Result<World, WorldError> {
        let level_file_name = LEVEL_FILE_NAMES
            .into_iter()
            .find(|name| path.join(name).is_file())
            .ok_or_else(|| WorldError::NotAWorld(path.display().to_string()))?;

        let level = read_nbt(&std::fs::read(path.join(level_file_name))?)?;
        check_level_data(&level, path)?;

        let mut world = World {
            file_name: file_name_of(path)?,
            path: path.to_path_buf(),
            zip_root: None,
            level,
            level_file_name,
            world_gen_settings: None,
            player: None,
        };
        world.load_external_data();
        Ok(world)
    }

    fn open_zip(path: &Path) -> Result<World, WorldError> {
        let mut archive = zip::ZipArchive::new(File::open(path)?)?;
        let names: Vec<String> = archive.file_names().map(str::to_string).collect();

        let root = if LEVEL_FILE_NAMES
            .iter()
            .any(|name| names.iter().any(|entry| entry == name))
        {
            String::new()
        } else {
            let mut tops: Vec<&str> = names
                .iter()
                .filter_map(|name| name.split('/').next())
                .filter(|top| !top.is_empty())
                .collect();
            tops.sort_unstable();
            tops.dedup();
            match tops.as_slice() {
                [only] => format!("{only}/"),
                _ => return Err(WorldError::NotAWorld(path.display().to_string())),
            }
        };

        let level_file_name = LEVEL_FILE_NAMES
            .into_iter()
            .find(|name| names.iter().any(|entry| *entry == format!("{root}{name}")))
            .ok_or_else(|| WorldError::NotAWorld(path.display().to_string()))?;

        let mut bytes = Vec::new();
        archive
            .by_name(&format!("{root}{level_file_name}"))?
            .read_to_end(&mut bytes)?;
        let level = read_nbt(&bytes)?;
        check_level_data(&level, path)?;

        let file_name = if root.is_empty() {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_string()
        } else {
            root.trim_end_matches('/').to_string()
        };

        Ok(World {
            path: path.to_path_buf(),
            zip_root: Some(root),
            file_name,
            level,
            level_file_name,
            world_gen_settings: None,
            player: None,
        })
    }

    fn load_external_data(&mut self) {
        let Some(data) = self.data() else { return };
        let has_inline_world_gen_settings = tag(data, "WorldGenSettings").is_some();
        let inline_player_uuid = tag(data, "Player")
            .is_none()
            .then(|| singleplayer_uuid(data))
            .flatten();

        if !has_inline_world_gen_settings {
            let path = self.path.join("data/minecraft/world_gen_settings.dat");
            if let Ok(bytes) = std::fs::read(&path) {
                match read_nbt(&bytes) {
                    Ok(value) if tag(&value, "data").is_some() => {
                        self.world_gen_settings = Some((path, value));
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("读取 {} 失败: {e}", path.display()),
                }
            }
        }

        if let Some(uuid) = inline_player_uuid {
            let path = self.path.join(format!("players/data/{uuid}.dat"));
            if let Ok(bytes) = std::fs::read(&path) {
                match read_nbt(&bytes) {
                    Ok(value) => self.player = Some((path, value)),
                    Err(e) => tracing::warn!("读取 {} 失败: {e}", path.display()),
                }
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn is_archive(&self) -> bool {
        self.zip_root.is_some()
    }

    fn data(&self) -> Option<&Value> {
        tag(&self.level, "Data")
    }

    fn data_mut(&mut self) -> Option<&mut Value> {
        tag_mut(&mut self.level, "Data")
    }

    pub fn name(&self) -> &str {
        self.data()
            .and_then(|data| as_str(tag(data, "LevelName")))
            .unwrap_or_default()
    }

    pub fn game_version(&self) -> Option<&str> {
        let version = tag(self.data()?, "Version")?;
        as_str(tag(version, "Name"))
    }

    pub fn last_played(&self) -> i64 {
        self.data()
            .and_then(|data| as_long(tag(data, "LastPlayed")))
            .unwrap_or(0)
    }

    pub fn icon_path(&self) -> Option<PathBuf> {
        if self.is_archive() {
            return None;
        }
        let icon = self.path.join("icon.png");
        icon.is_file().then_some(icon)
    }

    /// WorldGenSettings：1.16 之前不存在，1.16 起内嵌在 `Data` 里，更新的版本又
    /// 挪到了 `data/minecraft/world_gen_settings.dat` 的 `data` 子标签下。
    fn world_gen(&self) -> Option<&Value> {
        if let Some(inline) = self.data().and_then(|data| tag(data, "WorldGenSettings")) {
            return Some(inline);
        }
        self.world_gen_settings
            .as_ref()
            .and_then(|(_, root)| tag(root, "data"))
    }

    fn world_gen_mut(&mut self) -> Option<&mut Value> {
        if self
            .data()
            .and_then(|data| tag(data, "WorldGenSettings"))
            .is_some()
        {
            return self
                .data_mut()
                .and_then(|data| tag_mut(data, "WorldGenSettings"));
        }
        self.world_gen_settings
            .as_mut()
            .and_then(|(_, root)| tag_mut(root, "data"))
    }

    fn player_tag(&self) -> Option<&Value> {
        if let Some(inline) = self.data().and_then(|data| tag(data, "Player")) {
            return Some(inline);
        }
        self.player.as_ref().map(|(_, value)| value)
    }

    fn player_tag_mut(&mut self) -> Option<&mut Value> {
        if self.data().and_then(|data| tag(data, "Player")).is_some() {
            return self.data_mut().and_then(|data| tag_mut(data, "Player"));
        }
        self.player.as_mut().map(|(_, value)| value)
    }

    pub fn has_player_data(&self) -> bool {
        self.player_tag().is_some()
    }

    fn difficulty_settings(&self) -> Option<&Value> {
        tag(self.data()?, "difficulty_settings")
    }

    pub fn seed(&self) -> Option<i64> {
        // 1.16(20w20a) 起在 WorldGenSettings 里，之前在 Data.RandomSeed。
        if let Some(seed) = as_long(self.world_gen().and_then(|wgs| tag(wgs, "seed"))) {
            return Some(seed);
        }
        as_long(self.data().and_then(|data| tag(data, "RandomSeed")))
    }

    pub fn play_time_ticks(&self) -> Option<i64> {
        as_long(self.data().and_then(|data| tag(data, "Time")))
    }

    pub fn spawn_point(&self) -> Option<Location> {
        let data = self.data()?;
        if let Some(spawn) = tag(data, "spawn") {
            if let Some((x, y, z)) = coordinates(tag(spawn, "pos")) {
                return Some(Location {
                    dimension: dimension_name(tag(spawn, "dimension")).unwrap_or_default(),
                    x,
                    y,
                    z,
                });
            }
        }
        Some(Location {
            dimension: None,
            x: as_int(tag(data, "SpawnX"))? as f64,
            y: as_int(tag(data, "SpawnY"))? as f64,
            z: as_int(tag(data, "SpawnZ"))? as f64,
        })
    }

    pub fn player_location(&self) -> Option<Location> {
        let player = self.player_tag()?;
        let (x, y, z) = coordinates(tag(player, "Pos"))?;
        Some(Location {
            dimension: dimension_name(tag(player, "Dimension"))?,
            x,
            y,
            z,
        })
    }

    /// 22w14a 之前游戏不记录死亡地点，读不到就是没有。
    pub fn last_death_location(&self) -> Option<Location> {
        let death = tag(self.player_tag()?, "LastDeathLocation")?;
        let (x, y, z) = coordinates(tag(death, "pos"))?;
        Some(Location {
            dimension: dimension_name(tag(death, "dimension"))?,
            x,
            y,
            z,
        })
    }

    /// 床/重生锚位置。25w07a 起是 `respawn` 子标签，之前是 `SpawnX/Y/Z`
    /// （`SpawnDimension` 20w12a 才有，更早的版本只能重生在主世界）。
    pub fn player_respawn(&self) -> Option<Location> {
        let player = self.player_tag()?;
        if let Some(respawn) = tag(player, "respawn") {
            if let Some((x, y, z)) = coordinates(tag(respawn, "pos")) {
                return Some(Location {
                    dimension: dimension_name(tag(respawn, "dimension")).unwrap_or_default(),
                    x,
                    y,
                    z,
                });
            }
        }
        Some(Location {
            dimension: dimension_name(tag(player, "SpawnDimension")).unwrap_or_default(),
            x: as_int(tag(player, "SpawnX"))? as f64,
            y: as_int(tag(player, "SpawnY"))? as f64,
            z: as_int(tag(player, "SpawnZ"))? as f64,
        })
    }

    pub fn allow_commands(&self) -> Option<bool> {
        as_bool(self.data().and_then(|data| tag(data, "allowCommands")))
    }

    pub fn set_allow_commands(&mut self, value: bool) -> Result<(), WorldError> {
        let data = self
            .data_mut()
            .ok_or_else(|| WorldError::NotAWorld(String::new()))?;
        set_byte(data, "allowCommands", value);
        self.write_world_data()
    }

    /// 生成建筑：20w20a 之前是 `Data.MapFeatures`，之后在 WorldGenSettings 里，
    /// 26.1-snapshot-6 起字段又从 `generate_features` 改名成 `generate_structures`。
    pub fn generate_features(&self) -> Option<bool> {
        if let Some(value) = as_bool(self.data().and_then(|data| tag(data, "MapFeatures"))) {
            return Some(value);
        }
        let wgs = self.world_gen()?;
        as_bool(tag(wgs, "generate_features")).or_else(|| as_bool(tag(wgs, "generate_structures")))
    }

    pub fn set_generate_features(&mut self, value: bool) -> Result<(), WorldError> {
        if let Some(data) = self.data_mut() {
            if set_byte(data, "MapFeatures", value) {
                return self.write_world_data();
            }
        }
        if let Some(wgs) = self.world_gen_mut() {
            let _ = set_byte(wgs, "generate_features", value)
                || set_byte(wgs, "generate_structures", value);
        }
        self.write_world_data()
    }

    pub fn difficulty(&self) -> Option<Difficulty> {
        if let Some(Value::Byte(value)) = self.data().and_then(|data| tag(data, "Difficulty")) {
            return Difficulty::from_index(*value as usize);
        }
        let settings = self.difficulty_settings()?;
        Difficulty::from_id(as_str(tag(settings, "difficulty"))?)
    }

    pub fn set_difficulty(&mut self, difficulty: Difficulty) -> Result<(), WorldError> {
        if let Some(data) = self.data_mut() {
            match tag_mut(data, "Difficulty") {
                Some(slot @ Value::Byte(_)) => {
                    *slot = Value::Byte(difficulty.index() as i8);
                }
                _ => {
                    if let Some(settings) = tag_mut(data, "difficulty_settings") {
                        if let Some(slot @ Value::String(_)) = tag_mut(settings, "difficulty") {
                            *slot = Value::String(difficulty.id().to_string());
                        }
                    }
                }
            }
        }
        self.write_world_data()
    }

    pub fn difficulty_locked(&self) -> Option<bool> {
        if let Some(value) = as_bool(self.data().and_then(|data| tag(data, "DifficultyLocked"))) {
            return Some(value);
        }
        as_bool(tag(self.difficulty_settings()?, "locked"))
    }

    pub fn set_difficulty_locked(&mut self, value: bool) -> Result<(), WorldError> {
        if let Some(data) = self.data_mut() {
            if !set_byte(data, "DifficultyLocked", value) {
                if let Some(settings) = tag_mut(data, "difficulty_settings") {
                    set_byte(settings, "locked", value);
                }
            }
        }
        self.write_world_data()
    }

    fn hardcore(&self) -> Option<bool> {
        if let Some(value) = as_bool(self.data().and_then(|data| tag(data, "hardcore"))) {
            return Some(value);
        }
        as_bool(tag(self.difficulty_settings()?, "hardcore"))
    }

    pub fn game_type(&self) -> Option<GameType> {
        let raw = as_int(tag(self.player_tag()?, "playerGameType"))?;
        if self.hardcore() == Some(true) && raw == 0 {
            return Some(GameType::Hardcore);
        }
        match raw {
            0 => Some(GameType::Survival),
            1 => Some(GameType::Creative),
            2 => Some(GameType::Adventure),
            3 => Some(GameType::Spectator),
            _ => None,
        }
    }

    pub fn set_game_type(&mut self, game_type: GameType) -> Result<(), WorldError> {
        let hardcore = game_type == GameType::Hardcore;
        let raw = if hardcore {
            0
        } else {
            game_type.index() as i32
        };
        if let Some(player) = self.player_tag_mut() {
            set_int(player, "playerGameType", raw);
        }
        if let Some(data) = self.data_mut() {
            if !set_byte(data, "hardcore", hardcore) {
                if let Some(settings) = tag_mut(data, "difficulty_settings") {
                    set_byte(settings, "hardcore", hardcore);
                }
            }
        }
        self.write_world_data()
    }

    pub fn player_health(&self) -> Option<f32> {
        as_float(tag(self.player_tag()?, "Health"))
    }

    pub fn player_food_level(&self) -> Option<i32> {
        as_int(tag(self.player_tag()?, "foodLevel"))
    }

    pub fn player_food_saturation(&self) -> Option<f32> {
        as_float(tag(self.player_tag()?, "foodSaturationLevel"))
    }

    pub fn player_xp_level(&self) -> Option<i32> {
        as_int(tag(self.player_tag()?, "XpLevel"))
    }

    pub fn set_player_stats(
        &mut self,
        health: Option<f32>,
        food_level: Option<i32>,
        food_saturation: Option<f32>,
        xp_level: Option<i32>,
    ) -> Result<(), WorldError> {
        if let Some(player) = self.player_tag_mut() {
            if let Some(value) = health {
                set_float(player, "Health", value);
            }
            if let Some(value) = food_level {
                set_int(player, "foodLevel", value);
            }
            if let Some(value) = food_saturation {
                set_float(player, "foodSaturationLevel", value);
            }
            if let Some(value) = xp_level {
                set_int(player, "XpLevel", value);
            }
        }
        self.write_world_data()
    }

    /// 换世界图标。游戏只认 64×64 的 `icon.png`，尺寸不对会直接不显示，所以
    /// 调用方要先自己校验尺寸（core 层不解码图片）。
    pub fn set_icon(&self, source: &Path) -> Result<(), WorldError> {
        if self.is_archive() {
            return Err(WorldError::ReadOnly);
        }
        std::fs::copy(source, self.path.join("icon.png"))?;
        Ok(())
    }

    pub fn clear_icon(&self) -> Result<(), WorldError> {
        if self.is_archive() {
            return Err(WorldError::ReadOnly);
        }
        std::fs::remove_file(self.path.join("icon.png")).or_else(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(e)
            }
        })?;
        Ok(())
    }

    fn at_least(&self, version: &str) -> bool {
        self.game_version()
            .and_then(|current| GameVersionNumber::compare(current, version))
            .is_some_and(|ordering| ordering.is_ge())
    }

    pub fn supports_data_packs(&self) -> bool {
        self.at_least(MIN_DATA_PACK_VERSION)
    }

    pub fn supports_quick_play(&self) -> bool {
        self.at_least(MIN_QUICK_PLAY_VERSION)
    }

    pub fn session_lock_file(&self) -> PathBuf {
        self.path.join("session.lock")
    }

    /// 抢占世界的 `session.lock`，拿到就一直持有到返回的 [`File`] 被 drop。
    /// 编辑 `level.dat` 前必须先抢到——游戏正在跑的时候改存档会把世界写坏。
    pub fn lock(&self) -> Result<File, WorldError> {
        if self.is_archive() {
            return Err(WorldError::ReadOnly);
        }
        let mut file = File::options()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.session_lock_file())?;
        file.write_all("\u{2603}".as_bytes())?;
        file.sync_all()?;
        file.try_lock()
            .map_err(|_| WorldError::Locked(self.file_name.clone()))?;
        Ok(file)
    }

    /// 世界是否正被运行中的游戏占用。注意本进程自己持有锁的时候这里也返回
    /// `true`（Windows 的文件锁是按句柄算的），这跟原版行为一致。
    pub fn is_locked(&self) -> bool {
        if self.is_archive() {
            return false;
        }
        match File::options().write(true).open(self.session_lock_file()) {
            Ok(file) => file.try_lock().is_err(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            // 打不开（多半是被独占）就当锁着，宁可少给一次编辑机会也别写坏存档。
            Err(_) => true,
        }
    }

    fn ensure_unlocked(&self) -> Result<(), WorldError> {
        if self.is_locked() {
            return Err(WorldError::Locked(self.file_name.clone()));
        }
        Ok(())
    }

    pub fn write_level_data(&self) -> Result<(), WorldError> {
        if self.is_archive() {
            return Err(WorldError::ReadOnly);
        }
        write_nbt(&self.path.join(self.level_file_name), &self.level)
    }

    /// 写回 `level.dat` **以及**两个外置文件（新版本把 WorldGenSettings 和玩家
    /// 数据挪出去了）。改世界属性一律走这个，不要只写 level.dat。
    pub fn write_world_data(&self) -> Result<(), WorldError> {
        self.write_level_data()?;
        if let Some((path, value)) = &self.world_gen_settings {
            write_nbt(path, value)?;
        }
        if let Some((path, value)) = &self.player {
            write_nbt(path, value)?;
        }
        Ok(())
    }

    pub fn set_name(&mut self, name: &str) -> Result<(), WorldError> {
        let invalid = WorldError::NotAWorld(self.path.display().to_string());
        let data = self.data_mut().ok_or(invalid)?;
        match tag_mut(data, "LevelName") {
            Some(slot @ Value::String(_)) => *slot = Value::String(name.to_string()),
            _ => return Err(WorldError::NotAWorld(self.path.display().to_string())),
        }
        self.write_level_data()
    }

    pub fn rename(&mut self, new_name: &str) -> Result<(), WorldError> {
        check_world_name(new_name)?;
        self.ensure_unlocked()?;
        let target = self
            .path
            .parent()
            .ok_or_else(|| WorldError::NotAWorld(self.path.display().to_string()))?
            .join(new_name);
        if target != self.path && target.exists() {
            return Err(WorldError::AlreadyExists(new_name.to_string()));
        }

        self.set_name(new_name)?;
        if target != self.path {
            std::fs::rename(&self.path, &target)?;
            self.path = target;
            self.file_name = new_name.to_string();
        }
        Ok(())
    }

    pub fn install(&self, saves_dir: &Path, name: &str) -> Result<PathBuf, WorldError> {
        check_world_name(name)?;
        let target = saves_dir.join(name);
        if target.exists() {
            return Err(WorldError::AlreadyExists(name.to_string()));
        }
        std::fs::create_dir_all(saves_dir)?;

        match &self.zip_root {
            Some(root) => extract_zip_subtree(&self.path, root, &target)?,
            None => copy_world_directory(&self.path, &target)?,
        }

        World::open(&target)?.set_name(name)?;
        Ok(target)
    }

    pub fn export(&self, output: &Path) -> Result<(), WorldError> {
        if self.is_archive() {
            return Err(WorldError::ReadOnly);
        }
        let root_name = match self.name() {
            "" => self.file_name.as_str(),
            name => name,
        };
        zip_directory(&self.path, output, root_name)
    }

    /// 在同级目录复制一份。**不复制 `session.lock`**，否则新世界一出生就是
    /// "使用中"。
    pub fn copy_to(&self, new_name: &str) -> Result<PathBuf, WorldError> {
        if self.is_archive() {
            return Err(WorldError::ReadOnly);
        }
        check_world_name(new_name)?;
        self.ensure_unlocked()?;
        let saves_dir = self
            .path
            .parent()
            .ok_or_else(|| WorldError::NotAWorld(self.path.display().to_string()))?;
        self.install(saves_dir, new_name)
    }

    pub fn delete(self) -> Result<(), WorldError> {
        if self.is_archive() {
            return Err(WorldError::ReadOnly);
        }
        self.ensure_unlocked()?;
        std::fs::remove_dir_all(&self.path)?;
        Ok(())
    }

    pub fn backup(&self, backups_dir: &Path) -> Result<PathBuf, WorldError> {
        if self.is_archive() {
            return Err(WorldError::ReadOnly);
        }
        std::fs::create_dir_all(backups_dir)?;
        let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let base = format!("{stamp}_{}", self.file_name);

        for count in 0..256 {
            let suffix = if count == 0 {
                String::new()
            } else {
                format!(" {count}")
            };
            let output = backups_dir.join(format!("{base}{suffix}.zip"));
            if output.exists() {
                continue;
            }
            zip_directory(&self.path, &output, &self.file_name)?;
            return Ok(output);
        }
        Err(WorldError::AlreadyExists(base))
    }

    pub fn backups(&self, backups_dir: &Path) -> Vec<Backup> {
        let Ok(entries) = std::fs::read_dir(backups_dir) else {
            return Vec::new();
        };
        let mut backups: Vec<Backup> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_str()?;
                let (stamp, rest) = parse_backup_name(name, &self.file_name)?;
                Some(Backup {
                    time: stamp,
                    count: rest,
                    path,
                })
            })
            .collect();
        backups.sort_by(|a, b| b.time.cmp(&a.time).then(b.count.cmp(&a.count)));
        backups
    }

    pub fn list(saves_dir: &Path) -> Vec<World> {
        let Ok(entries) = std::fs::read_dir(saves_dir) else {
            return Vec::new();
        };
        let mut worlds: Vec<World> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| match World::open(&entry.path()) {
                Ok(world) => Some(world),
                Err(e) => {
                    tracing::warn!("跳过 {}: {e}", entry.path().display());
                    None
                }
            })
            .collect();
        worlds.sort_by_key(|world| std::cmp::Reverse(world.last_played()));
        worlds
    }
}

pub struct Backup {
    pub path: PathBuf,
    /// 文件名里的时间戳，形如 `2026-07-28_02-03-47`（本机时区，创建时写的）。
    pub time: String,
    pub count: u32,
}

fn parse_backup_name(file_name: &str, world_folder: &str) -> Option<(String, u32)> {
    let rest = file_name.strip_suffix(".zip")?;
    if rest.len() < 20 || !rest.is_char_boundary(19) {
        return None;
    }
    let (stamp, tail) = rest.split_at(19);
    if !stamp
        .chars()
        .all(|c| c.is_ascii_digit() || c == '-' || c == '_')
    {
        return None;
    }
    let tail = tail.strip_prefix('_')?;
    let suffix = tail.strip_prefix(world_folder)?;
    let count = match suffix {
        "" => 0,
        other => other.strip_prefix(' ')?.parse().ok()?,
    };
    Some((stamp.to_string(), count))
}

fn check_level_data(level: &Value, path: &Path) -> Result<(), WorldError> {
    let invalid = || WorldError::NotAWorld(path.display().to_string());
    let data = tag(level, "Data").ok_or_else(invalid)?;
    as_str(tag(data, "LevelName")).ok_or_else(invalid)?;
    as_long(tag(data, "LastPlayed")).ok_or_else(invalid)?;
    Ok(())
}

fn singleplayer_uuid(data: &Value) -> Option<String> {
    let Value::IntArray(array) = tag(data, "singleplayer_uuid")? else {
        return None;
    };
    let parts: &[i32] = array;
    let [a, b, c, d] = parts else { return None };
    let high = ((*a as u32 as u64) << 32) | (*b as u32 as u64);
    let low = ((*c as u32 as u64) << 32) | (*d as u32 as u64);
    Some(uuid::Uuid::from_u64_pair(high, low).to_string())
}

fn file_name_of(path: &Path) -> Result<String, WorldError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| WorldError::NotAWorld(path.display().to_string()))
}

/// 世界名同时也是文件夹名，所以按"文件名能不能用"来校验。故意**不**限制成
/// ASCII（跟实例 id 不同）——中文世界名是常态。
fn check_world_name(name: &str) -> Result<(), WorldError> {
    let invalid = || WorldError::InvalidName(name.to_string());
    if name.trim().is_empty() || name == "." || name == ".." {
        return Err(invalid());
    }
    if name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
        return Err(invalid());
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(invalid());
    }
    Ok(())
}

/// `session.lock` 是"这个世界正被占用"的标记，复制/备份时必须排除，
/// 否则副本一出生就是"使用中"。
const EXCLUDED_FROM_COPY: &str = "session.lock";

fn copy_world_directory(source: &Path, target: &Path) -> Result<(), WorldError> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == EXCLUDED_FROM_COPY {
            continue;
        }
        let from = entry.path();
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_world_directory(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn zip_directory(source: &Path, output: &Path, root_name: &str) -> Result<(), WorldError> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut writer = zip::ZipWriter::new(File::create(output)?);
    add_directory_to_zip(&mut writer, source, source, root_name)?;
    writer.finish()?;
    Ok(())
}

fn add_directory_to_zip(
    writer: &mut zip::ZipWriter<File>,
    root: &Path,
    directory: &Path,
    root_name: &str,
) -> Result<(), WorldError> {
    let mut entries: Vec<_> = std::fs::read_dir(directory)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if entry.file_name() == EXCLUDED_FROM_COPY {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            add_directory_to_zip(writer, root, &path, root_name)?;
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| WorldError::NotAWorld(path.display().to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        let options: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        writer.start_file(format!("{root_name}/{relative}"), options)?;
        std::io::copy(&mut File::open(&path)?, writer)?;
    }
    Ok(())
}

/// 把 zip 里 `prefix` 下面的子树解到 `target`，顺带做路径穿越检查
/// （`..` 组件直接拒绝，别人打的包不可信）。
fn extract_zip_subtree(archive_path: &Path, prefix: &str, target: &Path) -> Result<(), WorldError> {
    let mut archive = zip::ZipArchive::new(File::open(archive_path)?)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let Some(name) = entry.enclosed_name() else {
            return Err(WorldError::NotAWorld(entry.name().to_string()));
        };
        let name = name.to_string_lossy().replace('\\', "/");
        let Some(relative) = name.strip_prefix(prefix) else {
            continue;
        };
        if relative.is_empty() {
            continue;
        }
        let destination = target.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::io::copy(&mut entry, &mut File::create(&destination)?)?;
    }
    Ok(())
}

/// Unix 毫秒 -> 本机时区的 `yyyy-MM-dd HH:mm`，给 UI 显示"上次游戏时间"用。
pub fn format_timestamp_millis(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// 去掉世界名里的 `§x` 颜色/格式转义。原版是按转义渲染成富文本，我们只是
/// 剥掉——Slint 的 `Text` 没有富文本，显示成乱码不如显示成干净的纯文本。
pub fn strip_formatting_codes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{a7}' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn compound(entries: &[(&str, Value)]) -> Value {
        Value::Compound(HashMap::from_iter(
            entries
                .iter()
                .map(|(key, value)| (key.to_string(), value.clone())),
        ))
    }

    fn level_data(entries: &[(&str, Value)]) -> Value {
        let mut data = vec![
            ("LevelName", Value::String("测试世界".into())),
            ("LastPlayed", Value::Long(1_700_000_000_000)),
        ];
        data.extend(entries.iter().cloned());
        compound(&[("Data", compound(&data))])
    }

    fn write_world(dir: &Path, level: &Value) {
        std::fs::create_dir_all(dir).unwrap();
        write_nbt(&dir.join("level.dat"), level).unwrap();
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hmcl-rs-world-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn nbt_round_trip_preserves_tag_types() {
        let dir = temp_dir("round-trip");
        let level = level_data(&[
            ("allowCommands", Value::Byte(1)),
            ("Difficulty", Value::Byte(2)),
            ("Time", Value::Long(123_456)),
            ("SpawnX", Value::Int(-40)),
            ("RandomSeed", Value::Long(-8_000_000_000)),
        ]);
        write_world(&dir, &level);

        let world = World::open(&dir).unwrap();
        world.write_level_data().unwrap();
        let reloaded = read_nbt(&std::fs::read(dir.join("level.dat")).unwrap()).unwrap();

        assert_eq!(reloaded, level);
        let data = tag(&reloaded, "Data").unwrap();
        assert!(matches!(tag(data, "allowCommands"), Some(Value::Byte(1))));
        assert!(matches!(tag(data, "SpawnX"), Some(Value::Int(-40))));
    }

    #[test]
    fn reads_basic_fields_and_version_gates() {
        let dir = temp_dir("basics");
        write_world(
            &dir,
            &level_data(&[(
                "Version",
                compound(&[("Name", Value::String("1.20.1".into()))]),
            )]),
        );

        let world = World::open(&dir).unwrap();
        assert_eq!(world.name(), "测试世界");
        assert_eq!(world.game_version(), Some("1.20.1"));
        assert_eq!(world.last_played(), 1_700_000_000_000);
        assert!(world.supports_data_packs());
        assert!(world.supports_quick_play());
        assert!(!world.is_locked());
    }

    #[test]
    fn version_gates_are_closed_when_the_version_is_unknown() {
        let dir = temp_dir("no-version");
        write_world(&dir, &level_data(&[]));

        let world = World::open(&dir).unwrap();
        assert_eq!(world.game_version(), None);
        assert!(!world.supports_data_packs());
        assert!(!world.supports_quick_play());
    }

    #[test]
    fn a_directory_without_level_dat_is_not_a_world() {
        let dir = temp_dir("not-a-world");
        std::fs::write(dir.join("readme.txt"), "hi").unwrap();
        assert!(matches!(World::open(&dir), Err(WorldError::NotAWorld(_))));
    }

    #[test]
    fn rename_changes_both_the_level_name_and_the_folder() {
        let root = temp_dir("rename");
        let dir = root.join("old-folder");
        write_world(&dir, &level_data(&[]));

        let mut world = World::open(&dir).unwrap();
        world.rename("新名字").unwrap();

        assert!(!dir.exists());
        let renamed = World::open(&root.join("新名字")).unwrap();
        assert_eq!(renamed.name(), "新名字");
        assert_eq!(renamed.file_name(), "新名字");
    }

    #[test]
    fn export_then_install_round_trips_through_a_zip() {
        let root = temp_dir("export-install");
        let dir = root.join("source");
        write_world(&dir, &level_data(&[]));
        std::fs::write(dir.join("session.lock"), "x").unwrap();
        std::fs::create_dir_all(dir.join("region")).unwrap();
        std::fs::write(dir.join("region/r.0.0.mca"), "chunk").unwrap();

        let zip = root.join("exported.zip");
        World::open(&dir).unwrap().export(&zip).unwrap();

        let saves = root.join("saves");
        let installed = World::open(&zip)
            .unwrap()
            .install(&saves, "导入的世界")
            .unwrap();

        assert_eq!(installed, saves.join("导入的世界"));
        assert_eq!(
            std::fs::read_to_string(installed.join("region/r.0.0.mca")).unwrap(),
            "chunk"
        );
        assert!(!installed.join("session.lock").exists());
        assert_eq!(World::open(&installed).unwrap().name(), "导入的世界");
    }

    #[test]
    fn copy_skips_the_session_lock_and_refuses_to_overwrite() {
        let root = temp_dir("copy");
        let dir = root.join("original");
        write_world(&dir, &level_data(&[]));
        std::fs::write(dir.join("session.lock"), "x").unwrap();

        let world = World::open(&dir).unwrap();
        world.copy_to("副本").unwrap();
        assert!(!root.join("副本/session.lock").exists());
        assert_eq!(World::open(&root.join("副本")).unwrap().name(), "副本");

        assert!(matches!(
            world.copy_to("副本"),
            Err(WorldError::AlreadyExists(_))
        ));
    }

    #[test]
    fn backup_names_are_deduplicated_within_the_same_second() {
        let root = temp_dir("backup");
        let dir = root.join("world");
        write_world(&dir, &level_data(&[]));
        let backups = root.join("backups");

        let world = World::open(&dir).unwrap();
        let first = world.backup(&backups).unwrap();
        let second = world.backup(&backups).unwrap();

        assert_ne!(first, second);
        assert!(first
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("_world.zip"));
        assert_eq!(World::open(&first).unwrap().name(), "测试世界");
    }

    #[test]
    fn rejects_world_names_that_are_not_usable_as_folder_names() {
        for name in ["", "  ", "..", "a/b", "a\\b", "a:b", "trailing."] {
            assert!(check_world_name(name).is_err(), "{name:?} 应该被拒绝");
        }
        for name in ["世界", "My World", "a-b_c.d"] {
            assert!(check_world_name(name).is_ok(), "{name:?} 应该被接受");
        }
    }

    #[test]
    fn strips_section_sign_formatting_codes() {
        assert_eq!(strip_formatting_codes("§c红色§r世界"), "红色世界");
        assert_eq!(strip_formatting_codes("普通"), "普通");
    }
}
