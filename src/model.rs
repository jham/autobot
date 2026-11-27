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

/// One of a head unit's vane selects. Vanes pass straight through to the board; the arbiter
/// never touches them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Vane {
    Vertical,
    Horizontal,
}

impl Vane {
    pub const ALL: [Vane; 2] = [Vane::Vertical, Vane::Horizontal];

    /// The board's entity id for it, also used in autobot's MQTT topics.
    pub fn as_str(self) -> &'static str {
        match self {
            Vane::Vertical => "vertical_vane",
            Vane::Horizontal => "horizontal_vane",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Vane::Vertical => "Vertical vane",
            Vane::Horizontal => "Horizontal vane",
        }
    }

    pub fn from_id(s: &str) -> Option<Vane> {
        Vane::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// A select entity on the board.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Select {
    /// The current option, if the board has reported one.
    pub state: Option<String>,
    pub options: Vec<String>,
    /// The board marks it disabled by default (not every head unit has one).
    pub disabled_by_default: bool,
}

/// What a board reports besides its climate entity. A vane missing from `vanes` means the board
/// has no such select.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extras {
    pub vanes: std::collections::BTreeMap<Vane, Select>,
    /// Outside air temperature, as the outdoor unit reports it through this head unit.
    pub outside: Option<f64>,
}
