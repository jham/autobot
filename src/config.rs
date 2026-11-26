//! Configuration. Standalone installs use a TOML file (see `config.example.toml`); the Home
//! Assistant add-on gets the same structure as JSON in `/data/options.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::arbiter::{ArbiterConfig, IdleAction};

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MqttConfig {
    pub host: String,
    #[serde(default = "default_mqtt_port")]
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default = "default_discovery_prefix")]
    pub discovery_prefix: String,
    #[serde(default = "default_base_topic")]
    pub base_topic: String,
}

fn default_mqtt_port() -> u16 {
    1883
}
fn default_discovery_prefix() -> String {
    "homeassistant".into()
}
fn default_base_topic() -> String {
    "autobot".into()
}

/// Arbiter tuning as written in the config file: every field optional, times ending in `_min`
/// in minutes, other times in seconds.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArbiterOverrides {
    pub start_offset: Option<f64>,
    pub min_run_min: Option<f64>,
    pub max_slice_min: Option<f64>,
    pub idle_switch_delay_min: Option<f64>,
    pub demand_ratio: Option<f64>,
    pub changeover_settle: Option<f64>,
    pub dry_weight: Option<f64>,
    pub idle_action: Option<IdleAction>,
}

impl ArbiterOverrides {
    fn apply(&self, base: &ArbiterConfig) -> ArbiterConfig {
        ArbiterConfig {
            start_offset: self.start_offset.unwrap_or(base.start_offset),
            min_run: self.min_run_min.map_or(base.min_run, |m| m * 60.0),
            max_slice: self.max_slice_min.map_or(base.max_slice, |m| m * 60.0),
            idle_switch_delay: self
                .idle_switch_delay_min
                .map_or(base.idle_switch_delay, |m| m * 60.0),
            demand_ratio: self.demand_ratio.unwrap_or(base.demand_ratio),
            changeover_settle: self.changeover_settle.unwrap_or(base.changeover_settle),
            dry_weight: self.dry_weight.unwrap_or(base.dry_weight),
            idle_action: self.idle_action.unwrap_or(base.idle_action),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOutdoor {
    id: String,
    name: Option<String>,
    #[serde(flatten)]
    arbiter: ArbiterOverrides,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UnitConfig {
    pub id: String,
    pub name: String,
    /// Id of the `[[outdoor]]` this head unit is piped to.
    pub outdoor: String,
    /// IP address or host name of the esp105 board.
    pub host: String,
    #[serde(default = "default_api_port")]
    pub port: u16,
    /// The board's `api_encryption_key` (base64).
    pub encryption_key: String,
    /// `object_id` of the climate entity; only needed if the board has more than one.
    pub climate: Option<String>,
}

fn default_api_port() -> u16 {
    6053
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    tick: Option<f64>,
    state_file: Option<PathBuf>,
    adopt_external: Option<bool>,
    adopt_after: Option<f64>,
    auto_deadband: Option<f64>,
    min_temp: Option<f64>,
    max_temp: Option<f64>,
    log_level: Option<String>,
    mqtt: Option<MqttConfig>,
    #[serde(default)]
    arbiter: ArbiterOverrides,
    #[serde(default)]
    outdoor: Vec<RawOutdoor>,
    #[serde(default)]
    unit: Vec<UnitConfig>,
}

#[derive(Clone, Debug)]
pub struct OutdoorConfig {
    pub id: String,
    pub name: String,
    pub arbiter: ArbiterConfig,
    /// Ids of the head units on this outdoor unit, in config order.
    pub units: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub outdoor: Vec<OutdoorConfig>,
    pub units: BTreeMap<String, UnitConfig>,
    pub mqtt: Option<MqttConfig>,
    pub state_file: PathBuf,
    /// Seconds between arbiter steps. Reports and requests also wake it.
    pub tick: f64,
    pub adopt_external: bool,
    /// Seconds after a command before head-unit changes count as the user's.
    pub adopt_after: f64,
    pub auto_deadband: f64,
    pub min_temp: f64,
    pub max_temp: f64,
    pub log_level: Option<String>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl Config {
    /// Load a `.toml` or `.json` file. A relative `state_file` is relative to the config file.
    pub fn load(path: &Path) -> Result<Config> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let raw: RawConfig = if path.extension().is_some_and(|e| e == "json") {
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
        } else {
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
        };
        let base = path.parent().unwrap_or(Path::new("."));
        Self::from_raw(raw, base)
    }

    fn from_raw(raw: RawConfig, base: &Path) -> Result<Config> {
        let defaults = raw.arbiter.apply(&ArbiterConfig::default());
        let mut outdoor = Vec::new();
        let mut ids = BTreeSet::new();
        for o in raw.outdoor {
            if !valid_id(&o.id) {
                bail!(
                    "outdoor id {:?}: use lower-case letters, digits and _",
                    o.id
                );
            }
            if !ids.insert(o.id.clone()) {
                bail!("outdoor id {:?} is used twice", o.id);
            }
            let arbiter = o.arbiter.apply(&defaults);
            arbiter
                .validate()
                .map_err(|e| anyhow::anyhow!("outdoor {:?}: {e}", o.id))?;
            outdoor.push(OutdoorConfig {
                name: o.name.unwrap_or_else(|| o.id.clone()),
                id: o.id,
                arbiter,
                units: vec![],
            });
        }
        if outdoor.is_empty() {
            bail!("no outdoor units configured");
        }
        let mut units = BTreeMap::new();
        for u in raw.unit {
            if !valid_id(&u.id) {
                bail!("unit id {:?}: use lower-case letters, digits and _", u.id);
            }
            let Some(o) = outdoor.iter_mut().find(|o| o.id == u.outdoor) else {
                bail!("unit {:?}: no outdoor unit with id {:?}", u.id, u.outdoor);
            };
            o.units.push(u.id.clone());
            if units.insert(u.id.clone(), u.clone()).is_some() {
                bail!("unit id {:?} is used twice", u.id);
            }
        }
        if let Some(o) = outdoor.iter().find(|o| o.units.is_empty()) {
            bail!("outdoor unit {:?} has no head units", o.id);
        }
        let state_file = raw
            .state_file
            .unwrap_or_else(|| "autobot-state.json".into());
        let cfg = Config {
            outdoor,
            units,
            mqtt: raw.mqtt,
            state_file: if state_file.is_absolute() {
                state_file
            } else {
                base.join(state_file)
            },
            tick: raw.tick.unwrap_or(10.0),
            adopt_external: raw.adopt_external.unwrap_or(true),
            adopt_after: raw.adopt_after.unwrap_or(30.0),
            auto_deadband: raw.auto_deadband.unwrap_or(2.0),
            min_temp: raw.min_temp.unwrap_or(16.0),
            max_temp: raw.max_temp.unwrap_or(31.0),
            log_level: raw.log_level,
        };
        if cfg.tick <= 0.0
            || cfg.adopt_after < 0.0
            || cfg.auto_deadband < 0.0
            || cfg.min_temp >= cfg.max_temp
        {
            bail!("tick must be > 0, adopt_after and auto_deadband >= 0, min_temp < max_temp");
        }
        Ok(cfg)
    }
}
