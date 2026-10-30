use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const SCHEMA_VERSION: u64 = 1;
pub const THEME_PRESETS: &str = include_str!("../../ui/themes.json");

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub schema_version: u64,
    pub theme: ThemeSettings,
    pub orb: OrbSettings,
    pub motion: MotionSettings,
    pub material: MaterialSettings,
    pub layout: LayoutSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThemeSettings {
    pub selected_id: String,
    pub overrides: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThemeText {
    pub id: String,
    pub name: String,
    pub base_preset_id: String,
    pub overrides: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OrbSettings {
    pub metric: String,
    pub decimals: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MotionSettings {
    pub preset: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MaterialSettings {
    pub mode: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LayoutSettings {
    pub density: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            theme: ThemeSettings::default(),
            orb: OrbSettings::default(),
            motion: MotionSettings::default(),
            material: MaterialSettings::default(),
            layout: LayoutSettings::default(),
        }
    }
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            selected_id: "abyss".into(),
            overrides: Map::new(),
        }
    }
}

impl Default for ThemeText {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: "导入的主题".into(),
            base_preset_id: "abyss".into(),
            overrides: json!({}),
        }
    }
}

impl Default for OrbSettings {
    fn default() -> Self {
        Self {
            metric: "total".into(),
            decimals: 2,
        }
    }
}

impl Default for MotionSettings {
    fn default() -> Self {
        Self {
            preset: "lively".into(),
        }
    }
}

impl Default for MaterialSettings {
    fn default() -> Self {
        Self {
            mode: "auto".into(),
        }
    }
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            density: "compact".into(),
        }
    }
}

pub struct SettingsStore {
    path: PathBuf,
    pub settings: Settings,
    pub revision: u64,
    pub needs_migration: bool,
    pub read_only: bool,
    pub load_error: Option<String>,
    presets: Value,
}

pub struct SettingsState(pub Mutex<SettingsStore>);

impl SettingsState {
    pub fn load() -> Self {
        Self(Mutex::new(SettingsStore::load(default_path())))
    }
}

fn default_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kimi-quota-bar")
        .join("settings.json")
}

impl SettingsStore {
    pub fn load(path: PathBuf) -> Self {
        let presets = serde_json::from_str(THEME_PRESETS).unwrap_or_else(|_| json!([]));
        let mut store = Self {
            path,
            settings: Settings::default(),
            revision: 0,
            needs_migration: false,
            read_only: false,
            load_error: None,
            presets,
        };
        if !store.path.exists() {
            store.needs_migration = true;
            return store;
        }
        match read_settings(&store.path) {
            Ok(settings) => store.settings = settings,
            Err(LoadFailure::FutureVersion) => {
                store.read_only = true;
                store.load_error =
                    Some("设置由更新版本创建，当前应用以兼容模式运行且不会覆盖它".into());
            }
            Err(LoadFailure::Invalid(error)) => {
                let backup = store.path.with_extension("json.bak");
                match read_settings(&backup) {
                    Ok(settings) => {
                        store.settings = settings;
                        store.load_error = Some("主设置损坏，已恢复上一份有效备份".into());
                    }
                    Err(_) => {
                        store.load_error = Some(format!("设置读取失败，已使用默认值：{error}"));
                    }
                }
            }
        }
        store
    }

    pub fn config(&self) -> Value {
        json!({
            "layout": {
                "quota": { "width": 280, "height": 252 },
                "login": { "width": 280, "height": 400 },
                "profile": { "width": 280, "height": 320 },
                "appearance": { "width": 280, "height": 460 },
                "diagnostics": { "width": 280, "height": 400 },
                "ball": { "width": 60, "height": 60 }
            },
            "presets": self.presets,
            "settings": self.settings,
            "activeTheme": resolve_theme(&self.presets, &self.settings),
            "revision": self.revision,
            "needsMigration": self.needs_migration,
            "readOnly": self.read_only,
            "loadError": self.load_error,
        })
    }

    pub fn commit(&mut self, settings: Settings, expected_revision: u64) -> Result<Value, String> {
        if self.read_only {
            return Err("当前设置为只读，请升级应用后再保存".into());
        }
        if expected_revision != self.revision {
            return Err("设置已在其他入口变化，请重新载入后再保存".into());
        }
        validate(&self.presets, &settings)?;
        atomic_save(&self.path, &settings)?;
        self.settings = settings;
        self.revision += 1;
        self.needs_migration = false;
        self.load_error = None;
        Ok(json!({
            "ok": true,
            "settings": self.settings,
            "activeTheme": resolve_theme(&self.presets, &self.settings),
            "revision": self.revision,
        }))
    }

    pub fn migrate(
        &mut self,
        theme: Option<String>,
        metric: Option<String>,
        showcase: Option<bool>,
    ) -> Result<Value, String> {
        if self.path.exists() {
            self.needs_migration = false;
            return Ok(json!({ "ok": true, "settings": self.settings, "revision": self.revision }));
        }
        let mut next = Settings::default();
        if let Some(id) = theme.filter(|id| preset_ids(&self.presets).contains(id)) {
            next.theme.selected_id = id;
        }
        if let Some(metric) = metric.filter(|m| ["total", "5h", "7d"].contains(&m.as_str())) {
            next.orb.metric = metric;
        }
        next.motion.preset = if showcase == Some(true) {
            "stardust"
        } else {
            "lively"
        }
        .into();
        self.commit(next, self.revision)
    }
}

enum LoadFailure {
    FutureVersion,
    Invalid(String),
}

fn normalize_legacy_theme(value: &mut Value, presets: &Value) {
    let ids = preset_ids(presets);
    let Some(theme) = value.get_mut("theme").and_then(Value::as_object_mut) else {
        return;
    };
    let selected = theme
        .get("selectedId")
        .and_then(Value::as_str)
        .unwrap_or("abyss")
        .to_string();
    let legacy = theme.remove("customThemes");
    if ids.contains(&selected) {
        return;
    }
    let base = legacy
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| item["id"].as_str() == Some(selected.as_str()))
        .and_then(|item| item["basePresetId"].as_str())
        .filter(|id| ids.iter().any(|preset| preset == id));
    theme.insert("selectedId".into(), json!(base.unwrap_or("abyss")));
}

fn read_settings(path: &Path) -> Result<Settings, LoadFailure> {
    let raw = std::fs::read_to_string(path).map_err(|e| LoadFailure::Invalid(e.to_string()))?;
    let mut value: Value =
        serde_json::from_str(&raw).map_err(|e| LoadFailure::Invalid(e.to_string()))?;
    if value
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > SCHEMA_VERSION
    {
        return Err(LoadFailure::FutureVersion);
    }
    let presets = serde_json::from_str(THEME_PRESETS).unwrap_or(json!([]));
    normalize_legacy_theme(&mut value, &presets);
    let settings: Settings =
        serde_json::from_value(value).map_err(|e| LoadFailure::Invalid(e.to_string()))?;
    validate(&presets, &settings).map_err(LoadFailure::Invalid)?;
    Ok(settings)
}

fn atomic_save(path: &Path, settings: &Settings) -> Result<(), String> {
    let dir = path.parent().ok_or_else(|| "设置路径无效".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("无法创建设置目录：{e}"))?;
    let tmp = path.with_extension("json.tmp");
    let backup = path.with_extension("json.bak");
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, bytes).map_err(|e| format!("无法写入设置：{e}"))?;
    if path.exists() {
        std::fs::copy(path, &backup).map_err(|e| format!("无法备份设置：{e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("无法替换设置：{e}"))?;
    Ok(())
}

pub fn preset_ids(presets: &Value) -> Vec<String> {
    presets
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["id"].as_str().map(str::to_string))
        .collect()
}

fn valid_hex(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
    })
}

fn validate_overrides(overrides: &Value) -> Result<(), String> {
    let map = overrides
        .as_object()
        .ok_or_else(|| "主题覆盖值必须是对象".to_string())?;
    const COLORS: &[&str] = &[
        "background",
        "surface",
        "text",
        "secondary",
        "accent",
        "buttonText",
        "glow",
        "orbBackground",
        "orbText",
        "track",
        "border",
        "cardTrack",
        "selected",
        "focus",
        "statusNormal",
        "statusWarning",
        "statusCritical",
        "statusUnknown",
    ];
    for (key, value) in map {
        if COLORS.contains(&key.as_str()) && !valid_hex(value) {
            return Err(format!("{key} 必须是 #RRGGBB 色值"));
        }
        match key.as_str() {
            "gradient" => {
                let colors = value
                    .as_array()
                    .ok_or_else(|| "渐变色必须是数组".to_string())?;
                if !(2..=5).contains(&colors.len()) || !colors.iter().all(valid_hex) {
                    return Err("渐变色需包含 2–5 个合法色值".into());
                }
            }
            "particles" => {
                let colors = value
                    .as_array()
                    .ok_or_else(|| "粒子色必须是数组".to_string())?;
                if colors.len() != 2 || !colors.iter().all(valid_hex) {
                    return Err("粒子色需包含 2 个合法色值".into());
                }
            }
            "cardOpacity" | "orbOpacity" => {
                if !value.as_f64().is_some_and(|v| (0.05..=1.0).contains(&v)) {
                    return Err("透明度必须在 0.05–1.0 之间".into());
                }
            }
            "orbGlow" => {
                if !value.as_f64().is_some_and(|v| (0.0..=0.5).contains(&v)) {
                    return Err("光晕必须在 0–0.5 之间".into());
                }
            }
            "appearance" if !matches!(value.as_str(), Some("light" | "dark")) => {
                return Err("明暗基调无效".into());
            }
            _ if !COLORS.contains(&key.as_str())
                && !matches!(
                    key.as_str(),
                    "gradient"
                        | "particles"
                        | "cardOpacity"
                        | "orbOpacity"
                        | "orbGlow"
                        | "appearance"
                ) =>
            {
                return Err(format!("不支持的主题字段：{key}"));
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn validate(presets: &Value, settings: &Settings) -> Result<(), String> {
    if settings.schema_version != SCHEMA_VERSION {
        return Err("设置版本不受支持".into());
    }
    let ids = preset_ids(presets);
    for (id, overrides) in &settings.theme.overrides {
        if !ids.contains(id) {
            return Err("主题覆盖值对应内置主题不存在".into());
        }
        validate_overrides(overrides)?;
    }
    if !ids.contains(&settings.theme.selected_id) {
        return Err("选中的主题不存在".into());
    }
    if !["total", "5h", "7d"].contains(&settings.orb.metric.as_str()) {
        return Err("浮珠指标无效".into());
    }
    if settings.orb.decimals != 2 {
        return Err("本期浮珠固定显示两位小数".into());
    }
    if !["static", "lively", "stardust"].contains(&settings.motion.preset.as_str()) {
        return Err("动效档位无效".into());
    }
    if !["auto", "matte", "enhanced"].contains(&settings.material.mode.as_str()) {
        return Err("材质模式无效".into());
    }
    if settings.layout.density != "compact" {
        return Err("布局密度无效".into());
    }
    Ok(())
}

pub fn resolve_theme(presets: &Value, settings: &Settings) -> Value {
    let mut theme = presets
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|p| p["id"].as_str() == Some(settings.theme.selected_id.as_str()))
        })
        .cloned()
        .unwrap_or_else(|| presets[0].clone());
    if let Some(overrides) = settings
        .theme
        .overrides
        .get(&settings.theme.selected_id)
        .and_then(Value::as_object)
    {
        for (key, value) in overrides {
            match key.as_str() {
                "appearance" => theme["appearance"] = value.clone(),
                "cardOpacity" | "orbOpacity" | "orbGlow" => theme["material"][key] = value.clone(),
                "statusNormal" | "statusWarning" | "statusCritical" | "statusUnknown" => {
                    let status_key = key.trim_start_matches("status").to_lowercase();
                    theme["status"][status_key] = value.clone();
                }
                _ => theme["palette"][key] = value.clone(),
            }
        }
    }
    theme
}

pub fn settings_from_value(mut value: Value) -> Result<Settings, String> {
    let presets = serde_json::from_str(THEME_PRESETS).unwrap_or(json!([]));
    normalize_legacy_theme(&mut value, &presets);
    serde_json::from_value(value).map_err(|e| format!("设置格式无效：{e}"))
}

pub fn merged_theme_text(input: &str) -> Result<ThemeText, String> {
    if input.len() > 16 * 1024 {
        return Err("主题文本超过 16KiB".into());
    }
    let value: Value =
        serde_json::from_str(input).map_err(|e| format!("主题文本不是有效 JSON：{e}"))?;
    if value.get("schemaVersion").and_then(Value::as_u64) != Some(SCHEMA_VERSION) {
        return Err("主题文本版本不受支持".into());
    }
    let map = value
        .as_object()
        .ok_or_else(|| "主题文本必须是对象".to_string())?;
    let allowed = ["schemaVersion", "name", "basePresetId", "overrides"];
    if map.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err("主题文本包含未允许字段".into());
    }
    let theme = ThemeText {
        id: String::new(),
        name: value["name"].as_str().unwrap_or("导入的主题").to_string(),
        base_preset_id: value["basePresetId"]
            .as_str()
            .unwrap_or("abyss")
            .to_string(),
        overrides: value.get("overrides").cloned().unwrap_or_else(|| json!({})),
    };
    validate_overrides(&theme.overrides)?;
    if theme.name.trim().is_empty() || theme.name.chars().count() > 24 {
        return Err("主题名称需为 1–24 个字符".into());
    }
    Ok(theme)
}

pub fn export_theme(theme: &ThemeText) -> String {
    let mut map = Map::new();
    map.insert("schemaVersion".into(), json!(SCHEMA_VERSION));
    map.insert("name".into(), json!(theme.name));
    map.insert("basePresetId".into(), json!(theme.base_preset_id));
    map.insert("overrides".into(), theme.overrides.clone());
    serde_json::to_string_pretty(&map).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("kqb-{name}-{}", rand::random::<u64>()))
            .join("settings.json")
    }

    #[test]
    fn defaults_are_lively_and_valid() {
        let presets: Value = serde_json::from_str(THEME_PRESETS).unwrap();
        assert_eq!(presets.as_array().unwrap().len(), 8);
        let settings = Settings::default();
        assert_eq!(settings.theme.selected_id, "abyss");
        assert_eq!(settings.motion.preset, "lively");
        assert!(settings.theme.overrides.is_empty());
        validate(&presets, &settings).unwrap();
    }

    #[test]
    fn invalid_override_is_rejected_without_writing() {
        let path = temp_path("invalid");
        let mut store = SettingsStore::load(path.clone());
        let mut next = Settings::default();
        next.theme
            .overrides
            .insert("abyss".into(), json!({ "accent": "red" }));
        assert!(store.commit(next, 0).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn legacy_custom_records_are_cleared_without_resetting_motion() {
        let value = json!({
            "schemaVersion": 1,
            "theme": {
                "selectedId": "custom-old",
                "customThemes": [{
                    "id": "custom-old",
                    "name": "旧主题",
                    "basePresetId": "polar",
                    "overrides": { "accent": "#102030" }
                }]
            },
            "motion": { "preset": "stardust" }
        });
        let settings = settings_from_value(value).unwrap();
        assert_eq!(settings.theme.selected_id, "polar");
        assert!(settings.theme.overrides.is_empty());
        assert_eq!(settings.motion.preset, "stardust");
        let saved = serde_json::to_value(settings).unwrap();
        assert!(saved["theme"].get("customThemes").is_none());
    }

    #[test]
    fn each_preset_resolves_only_its_own_overrides() {
        let presets: Value = serde_json::from_str(THEME_PRESETS).unwrap();
        let mut settings = Settings::default();
        settings.theme.overrides.insert(
            "abyss".into(),
            json!({ "accent": "#102030", "orbOpacity": 0.3 }),
        );
        settings
            .theme
            .overrides
            .insert("polar".into(), json!({ "accent": "#304050" }));
        assert_eq!(
            resolve_theme(&presets, &settings)["palette"]["accent"],
            "#102030"
        );
        settings.theme.selected_id = "polar".into();
        assert_eq!(
            resolve_theme(&presets, &settings)["palette"]["accent"],
            "#304050"
        );
        settings.theme.selected_id = "abyss".into();
        assert_eq!(
            resolve_theme(&presets, &settings)["palette"]["accent"],
            "#102030"
        );
    }

    #[test]
    fn revision_conflict_does_not_replace_saved_settings() {
        let path = temp_path("revision");
        let mut store = SettingsStore::load(path.clone());
        store.commit(Settings::default(), 0).unwrap();
        let mut next = Settings::default();
        next.theme.selected_id = "polar".into();
        assert!(store.commit(next, 0).unwrap_err().contains("其他入口"));
        assert_eq!(store.settings.theme.selected_id, "abyss");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn migration_preserves_old_choices() {
        let path = temp_path("migration");
        let mut store = SettingsStore::load(path.clone());
        let result = store
            .migrate(Some("rainbow".into()), Some("7d".into()), Some(true))
            .unwrap();
        assert_eq!(result["settings"]["theme"]["selectedId"], "rainbow");
        assert_eq!(result["settings"]["orb"]["metric"], "7d");
        assert_eq!(result["settings"]["motion"]["preset"], "stardust");
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn proto_zero_and_invalid_theme_values_are_distinct() {
        assert!(validate_overrides(&json!({ "gradient": ["#000000", "#FFFFFF"] })).is_ok());
        assert!(validate_overrides(&json!({ "gradient": ["#000000"] })).is_err());
        assert!(validate_overrides(&json!({ "orbOpacity": 0.05 })).is_ok());
        assert!(validate_overrides(&json!({ "cardOpacity": 0.05 })).is_ok());
        assert!(validate_overrides(&json!({ "orbOpacity": 0.049 })).is_err());
    }
}
