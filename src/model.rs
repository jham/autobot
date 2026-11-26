//! Plain data types shared by the arbiter, the head-unit backends and the frontends.
//!
//! Temperatures are degrees Celsius throughout. ESPHome climate entities always speak Celsius,
//! and frontends convert if they need to.

use serde::{Deserialize, Serialize};

/// Operating mode of a unit, as requested by a user or as commanded to a head unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Off,
    Heat,
    Cool,
    /// Dual setpoint; autobot picks heat or cool. Never sent to a head unit.
    Auto,
    Dry,
    FanOnly,
}

/// Which way the outdoor unit is running. A multi-split outdoor unit does one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Heat,
    Cool,
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::Heat => Side::Cool,
            Side::Cool => Side::Heat,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Side::Heat => "heat",
            Side::Cool => "cool",
        }
    }
}

/// What a unit is doing right now. Names match Home Assistant's climate `hvac_action`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Off,
    Heating,
    Cooling,
    Drying,
    Idle,
    Fan,
    Defrosting,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Off => "off",
            Action::Heating => "heating",
            Action::Cooling => "cooling",
            Action::Drying => "drying",
            Action::Idle => "idle",
            Action::Fan => "fan",
            Action::Defrosting => "defrosting",
        }
    }
}

/// What the user asked a virtual unit to do (from Home Assistant, Matter, ...).
///
/// `target` is used in HEAT, COOL and DRY. `low` and `high` are the heating and cooling
/// setpoints used in AUTO. Fan and swing are passed through to the head unit untouched, as
/// strings in Home Assistant's vocabulary ("auto", "quiet", "low", ...; "off", "vertical").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UnitRequest {
    pub mode: Mode,
    pub target: f64,
    pub low: f64,
    pub high: f64,
    pub fan: Option<String>,
    pub swing: Option<String>,
}

impl Default for UnitRequest {
    fn default() -> Self {
        UnitRequest {
            mode: Mode::Off,
            target: 21.0,
            low: 20.0,
            high: 24.0,
            fan: None,
            swing: None,
        }
    }
}

impl UnitRequest {
    pub fn auto(low: f64, high: f64) -> Self {
        UnitRequest {
            mode: Mode::Auto,
            low,
            high,
            ..Default::default()
        }
    }

    pub fn mode(mode: Mode, target: f64) -> Self {
        UnitRequest {
            mode,
            target,
            ..Default::default()
        }
    }
}

/// Latest state reported by a head unit's esp105.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnitReport {
    pub available: bool,
    pub mode: Option<Mode>,
    pub action: Option<Action>,
    /// Room temperature.
    pub current: Option<f64>,
    pub target: Option<f64>,
    pub fan: Option<String>,
    pub swing: Option<String>,
}

impl UnitReport {
    /// An available unit reporting a room temperature; the rest is filled in with struct update.
    pub fn at(current: f64) -> Self {
        UnitReport {
            available: true,
            current: Some(current),
            ..Default::default()
        }
    }
}

/// What autobot wants a head unit set to. `None` fields are left as they are.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitCommand {
    pub mode: Mode,
    pub target: Option<f64>,
    pub fan: Option<String>,
    pub swing: Option<String>,
}

impl UnitCommand {
    pub fn new(mode: Mode, target: Option<f64>) -> Self {
        UnitCommand {
            mode,
            target,
            fan: None,
            swing: None,
        }
    }
}
