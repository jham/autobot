//! Head-unit backend for esp105 boards running the Wi-Fi (ESPHome) firmware.
//!
//! Talks to each board over the ESPHome native API (Noise-encrypted), the way Home Assistant
//! does. Add the boards to autobot instead of to Home Assistant's ESPHome integration: two
//! controllers writing the same climate entity will fight.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use esphome_client::EspHomeClient;
use esphome_client::types::{
    ClimateAction, ClimateCommandRequest, ClimateFanMode, ClimateMode, ClimateStateResponse,
    ClimateSwingMode, DisconnectResponse, EspHomeMessage, ListEntitiesClimateResponse,
    ListEntitiesRequest, PingRequest, SelectCommandRequest, SubscribeStatesRequest,
};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::config::UnitConfig;
use crate::model::{Action, Extras, Mode, Select, UnitCommand, UnitReport, Vane};
use crate::service::{Backend, Event};

/// The board's outside air temperature sensor (cn105's `outside_air_temperature_sensor`).
const OUTSIDE: &str = "outside_air_temperature";

/// Whether a listed entity is the one called `want` (an ESPHome object_id such as
/// "vertical_vane"). Newer ESPHome sends an empty object_id, so the name counts too.
fn is_entity(object_id: &str, name: &str, want: &str) -> bool {
    object_id == want || name.eq_ignore_ascii_case(want) || slug(name) == want
}

const PING_EVERY: Duration = Duration::from_secs(20);
/// No message at all (states, pings, pongs) for this long means the connection is dead.
const SILENCE_LIMIT: Duration = Duration::from_secs(90);

fn mode_in(m: i32) -> Option<Mode> {
    Some(match ClimateMode::try_from(m).ok()? {
        ClimateMode::Off => Mode::Off,
        ClimateMode::Heat => Mode::Heat,
        ClimateMode::Cool => Mode::Cool,
        ClimateMode::Auto | ClimateMode::HeatCool => Mode::Auto,
        ClimateMode::Dry => Mode::Dry,
        ClimateMode::FanOnly => Mode::FanOnly,
    })
}

fn mode_out(m: Mode) -> ClimateMode {
    match m {
        Mode::Off => ClimateMode::Off,
        Mode::Heat => ClimateMode::Heat,
        Mode::Cool => ClimateMode::Cool,
        Mode::Dry => ClimateMode::Dry,
        Mode::FanOnly => ClimateMode::FanOnly,
        // Resolving AUTO is autobot's whole job; the arbiter never produces it.
        Mode::Auto => unreachable!("AUTO is never sent to a head unit"),
    }
}

fn action_in(a: i32) -> Option<Action> {
    Some(match ClimateAction::try_from(a).ok()? {
        ClimateAction::Off => Action::Off,
        ClimateAction::Heating => Action::Heating,
        ClimateAction::Cooling => Action::Cooling,
        ClimateAction::Drying => Action::Drying,
        ClimateAction::Idle => Action::Idle,
        ClimateAction::Fan => Action::Fan,
        ClimateAction::Defrosting => Action::Defrosting,
    })
}

const FANS: [(ClimateFanMode, &str); 10] = [
    (ClimateFanMode::ClimateFanOn, "on"),
    (ClimateFanMode::ClimateFanOff, "off"),
    (ClimateFanMode::ClimateFanAuto, "auto"),
    (ClimateFanMode::ClimateFanLow, "low"),
    (ClimateFanMode::ClimateFanMedium, "medium"),
    (ClimateFanMode::ClimateFanHigh, "high"),
    (ClimateFanMode::ClimateFanMiddle, "middle"),
    (ClimateFanMode::ClimateFanFocus, "focus"),
    (ClimateFanMode::ClimateFanDiffuse, "diffuse"),
    (ClimateFanMode::ClimateFanQuiet, "quiet"),
];
const SWINGS: [(ClimateSwingMode, &str); 4] = [
    (ClimateSwingMode::ClimateSwingOff, "off"),
    (ClimateSwingMode::ClimateSwingBoth, "both"),
    (ClimateSwingMode::ClimateSwingVertical, "vertical"),
    (ClimateSwingMode::ClimateSwingHorizontal, "horizontal"),
];

fn name_of<E: Copy + Into<i32>>(table: &[(E, &'static str)], v: i32) -> Option<&'static str> {
    table
        .iter()
        .find(|(e, _)| (*e).into() == v)
        .map(|(_, n)| *n)
}

fn value_of<E: Copy + Into<i32>>(table: &[(E, &'static str)], name: &str) -> Option<i32> {
    table
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(e, _)| (*e).into())
}

/// ESPHome's object_id rule: lower-case, anything not alphanumeric becomes `_`.
fn slug(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn finite(x: f32) -> Option<f64> {
    x.is_finite().then_some(f64::from(x))
}

fn report_from(s: &ClimateStateResponse, modes: &Modes) -> UnitReport {
    // A state field is 0 when unset, which is also a valid enum value; trust only listed modes.
    let listed = |name: Option<&str>, list: &[String]| {
        name.filter(|n| list.iter().any(|m| m == n))
            .map(String::from)
    };
    UnitReport {
        available: true,
        mode: mode_in(s.mode),
        action: action_in(s.action),
        current: finite(s.current_temperature),
        target: finite(s.target_temperature).filter(|_| !modes.two_point),
        fan: listed(name_of(&FANS, s.fan_mode), &modes.fan),
        swing: listed(name_of(&SWINGS, s.swing_mode), &modes.swing),
    }
}

fn command_to(key: u32, cmd: &UnitCommand) -> ClimateCommandRequest {
    let mut req = ClimateCommandRequest {
        key,
        has_mode: true,
        mode: mode_out(cmd.mode).into(),
        ..Default::default()
    };
    if let Some(t) = cmd.target {
        req.has_target_temperature = true;
        req.target_temperature = t as f32;
    }
    if let Some(v) = cmd.fan.as_deref().and_then(|f| value_of(&FANS, f)) {
        req.has_fan_mode = true;
        req.fan_mode = v;
    }
    if let Some(v) = cmd.swing.as_deref().and_then(|s| value_of(&SWINGS, s)) {
        req.has_swing_mode = true;
        req.swing_mode = v;
    }
    req
}

#[derive(Clone, Default)]
struct Modes {
    fan: Vec<String>,
    swing: Vec<String>,
    /// The entity uses low/high setpoints; its single `target_temperature` means nothing.
    two_point: bool,
}

enum Command {
    Climate(UnitCommand),
    Vane(Vane, String),
}

/// One esp105 board. Keeps a connection open in a background task, reconnecting with backoff.
pub struct EspHomeUnit {
    report: watch::Receiver<UnitReport>,
    modes: Arc<Mutex<Modes>>,
    extras: Arc<Mutex<Extras>>,
    commands: mpsc::UnboundedSender<Command>,
}

impl EspHomeUnit {
    pub fn spawn(cfg: UnitConfig, wake: mpsc::UnboundedSender<Event>) -> Self {
        let (report_tx, report) = watch::channel(UnitReport::default());
        let (commands, command_rx) = mpsc::unbounded_channel();
        let modes = Arc::new(Mutex::new(Modes {
            // What esp105.yaml supports, until the board says otherwise.
            fan: ["auto", "quiet", "low", "medium", "high"]
                .map(String::from)
                .to_vec(),
            swing: ["off", "vertical"].map(String::from).to_vec(),
            two_point: false,
        }));
        let extras = Arc::new(Mutex::new(Extras::default()));
        tokio::spawn(run(
            cfg,
            report_tx,
            command_rx,
            modes.clone(),
            extras.clone(),
            wake,
        ));
        EspHomeUnit {
            report,
            modes,
            extras,
            commands,
        }
    }
}

impl Backend for EspHomeUnit {
    fn report(&self) -> UnitReport {
        self.report.borrow().clone()
    }

    fn fan_modes(&self) -> Vec<String> {
        self.modes.lock().expect("modes").fan.clone()
    }

    fn swing_modes(&self) -> Vec<String> {
        self.modes.lock().expect("modes").swing.clone()
    }

    fn send(&mut self, cmd: &UnitCommand) -> bool {
        self.report.borrow().available && self.commands.send(Command::Climate(cmd.clone())).is_ok()
    }

    fn extras(&self) -> Extras {
        self.extras.lock().expect("extras").clone()
    }

    fn set_vane(&mut self, vane: Vane, option: &str) -> bool {
        let known = self
            .extras
            .lock()
            .expect("extras")
            .vanes
            .get(&vane)
            .is_some_and(|s| s.options.iter().any(|o| o == option));
        known
            && self.report.borrow().available
            && self
                .commands
                .send(Command::Vane(vane, option.to_string()))
                .is_ok()
    }
}

async fn run(
    cfg: UnitConfig,
    report: watch::Sender<UnitReport>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    modes: Arc<Mutex<Modes>>,
    extras: Arc<Mutex<Extras>>,
    wake: mpsc::UnboundedSender<Event>,
) {
    let mut delay = Duration::from_secs(5);
    loop {
        let mut connected = false;
        let links = Links {
            report: &report,
            modes: &modes,
            extras: &extras,
            wake: &wake,
        };
        match session(&cfg, &links, &mut commands, &mut connected).await {
            Ok(()) => warn!("{}: disconnected", cfg.id),
            Err(e) => {
                // esphome-client errors repeat their source in their message; show the ends only.
                let root = e.root_cause().to_string();
                let msg = if root == e.to_string() {
                    root
                } else {
                    format!("{e}: {root}")
                };
                warn!("{}: {msg}; retrying in {}s", cfg.id, delay.as_secs());
            }
        }
        report.send_replace(UnitReport::default());
        {
            // Keep the options, so the entities stay put while the board is away.
            let mut x = extras.lock().expect("extras");
            x.outside = None;
            for s in x.vanes.values_mut() {
                s.state = None;
            }
        }
        let _ = wake.send(Event::Wake);
        // Commands queued while the link was down are stale; the service resends what it wants.
        while commands.try_recv().is_ok() {}
        if connected {
            delay = Duration::from_secs(5);
        }
        tokio::time::sleep(delay).await;
        if !connected {
            delay = (delay * 2).min(Duration::from_secs(300));
        }
    }
}

/// Where a session puts what it hears.
struct Links<'a> {
    report: &'a watch::Sender<UnitReport>,
    modes: &'a Mutex<Modes>,
    extras: &'a Mutex<Extras>,
    wake: &'a mpsc::UnboundedSender<Event>,
}

async fn session(
    cfg: &UnitConfig,
    links: &Links<'_>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    connected: &mut bool,
) -> Result<()> {
    let Links {
        report,
        modes,
        extras,
        wake,
    } = *links;
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let mut client = EspHomeClient::builder()
        .address(&addr)
        .key(&cfg.encryption_key)
        .client_info("autobot")
        .timeout(Duration::from_secs(10))
        .connect()
        .await
        .with_context(|| format!("connecting to {addr}"))?;

    client.try_write(ListEntitiesRequest {}).await?;
    let mut climates: Vec<ListEntitiesClimateResponse> = Vec::new();
    // Select key -> vane, and the vanes' options.
    let mut vane_keys: BTreeMap<u32, Vane> = BTreeMap::new();
    let mut vanes: BTreeMap<Vane, Select> = BTreeMap::new();
    let mut outside_key = None;
    loop {
        match tokio::time::timeout(Duration::from_secs(30), client.try_read())
            .await
            .context("listing entities")??
        {
            EspHomeMessage::ListEntitiesClimateResponse(c) => climates.push(c),
            EspHomeMessage::ListEntitiesSelectResponse(s) => {
                if let Some(v) = Vane::ALL
                    .into_iter()
                    .find(|v| is_entity(&s.object_id, &s.name, v.as_str()))
                {
                    vane_keys.insert(s.key, v);
                    vanes.insert(
                        v,
                        Select {
                            state: None,
                            options: s.options,
                            disabled_by_default: s.disabled_by_default,
                        },
                    );
                }
            }
            EspHomeMessage::ListEntitiesSensorResponse(s)
                if is_entity(&s.object_id, &s.name, OUTSIDE) =>
            {
                outside_key = Some(s.key);
            }
            EspHomeMessage::ListEntitiesDoneResponse(_) => break,
            _ => {}
        }
    }
    *extras.lock().expect("extras") = Extras {
        vanes,
        outside: None,
    };
    if let Some(want) = &cfg.climate {
        // Newer ESPHome sends an empty object_id, so match the name too ("Heat pump" or "heat_pump").
        climates.retain(|c| is_entity(&c.object_id, &c.name, want));
    }
    let [climate] = climates.as_slice() else {
        bail!(
            "expected one climate entity, found {:?}",
            climates.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    };
    let key = climate.key;
    {
        let mut m = modes.lock().expect("modes");
        m.fan = climate
            .supported_fan_modes
            .iter()
            .filter_map(|v| name_of(&FANS, *v))
            .map(String::from)
            .collect();
        m.swing = climate
            .supported_swing_modes
            .iter()
            .filter_map(|v| name_of(&SWINGS, *v))
            .map(String::from)
            .collect();
        m.two_point = climate.supports_two_point_target_temperature;
    }
    if climate.supports_two_point_target_temperature {
        warn!(
            "{}: climate {:?} uses low/high setpoints; autobot only sets a single target",
            cfg.id, climate.name
        );
    }
    client.try_write(SubscribeStatesRequest {}).await?;
    info!("{}: connected to {addr}", cfg.id);
    *connected = true;

    // try_read isn't cancel-safe, so reading gets its own task and hands messages over.
    let writer = client.write_stream();
    let (msg_tx, mut msgs) = mpsc::channel(64);
    let reader = tokio::spawn(async move {
        loop {
            let msg = client.try_read().await;
            let failed = msg.is_err();
            if msg_tx.send(msg).await.is_err() || failed {
                return;
            }
        }
    });
    let _abort = AbortOnDrop(reader);

    let mut ping = tokio::time::interval(PING_EVERY);
    let mut last_heard = tokio::time::Instant::now();
    loop {
        tokio::select! {
            msg = msgs.recv() => {
                let msg = msg.ok_or_else(|| anyhow!("connection reader stopped"))??;
                last_heard = tokio::time::Instant::now();
                match msg {
                    EspHomeMessage::ClimateStateResponse(s) if s.key == key => {
                        let r = report_from(&s, &modes.lock().expect("modes"));
                        debug!("{}: {r:?}", cfg.id);
                        report.send_replace(r);
                        let _ = wake.send(Event::Wake);
                    }
                    EspHomeMessage::SelectStateResponse(s) => {
                        let Some(v) = vane_keys.get(&s.key) else { continue };
                        let state = (!s.missing_state).then_some(s.state);
                        if let Some(sel) = extras.lock().expect("extras").vanes.get_mut(v) {
                            sel.state = state;
                        }
                        let _ = wake.send(Event::Wake);
                    }
                    EspHomeMessage::SensorStateResponse(s) if Some(s.key) == outside_key => {
                        let t = finite(s.state).filter(|_| !s.missing_state);
                        let mut x = extras.lock().expect("extras");
                        if x.outside != t {
                            x.outside = t;
                            drop(x);
                            let _ = wake.send(Event::Wake);
                        }
                    }
                    EspHomeMessage::DisconnectRequest(_) => {
                        let _ = writer.try_write(DisconnectResponse {}).await;
                        return Ok(());
                    }
                    _ => {}
                }
            }
            Some(cmd) = commands.recv() => match cmd {
                Command::Climate(cmd) => writer.try_write(command_to(key, &cmd)).await?,
                Command::Vane(v, option) => {
                    if let Some((&k, _)) = vane_keys.iter().find(|(_, x)| **x == v) {
                        writer
                            .try_write(SelectCommandRequest { key: k, state: option, ..Default::default() })
                            .await?;
                    }
                }
            },
            _ = ping.tick() => {
                if last_heard.elapsed() > SILENCE_LIMIT {
                    bail!("no reply for {}s", SILENCE_LIMIT.as_secs());
                }
                writer.try_write(PingRequest {}).await?;
            }
        }
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_sets_only_what_is_given() {
        let req = command_to(
            7,
            &UnitCommand {
                fan: Some("quiet".into()),
                ..UnitCommand::new(Mode::Heat, Some(20.5))
            },
        );
        assert_eq!(req.key, 7);
        assert!(req.has_mode && req.mode == i32::from(ClimateMode::Heat));
        assert!(req.has_target_temperature && req.target_temperature == 20.5);
        assert!(req.has_fan_mode && req.fan_mode == i32::from(ClimateFanMode::ClimateFanQuiet));
        assert!(!req.has_swing_mode);
    }

    #[test]
    fn slugs_match_esphome() {
        assert_eq!(slug("Heat pump"), "heat_pump");
        assert!(is_entity("", "Vertical vane", Vane::Vertical.as_str()));
        assert!(is_entity("", "Outside air temperature", OUTSIDE));
        assert!(!is_entity("", "Horizontal vane", Vane::Vertical.as_str()));
    }

    #[test]
    fn state_maps_to_report() {
        let s = ClimateStateResponse {
            key: 1,
            mode: ClimateMode::HeatCool.into(),
            action: ClimateAction::Heating.into(),
            current_temperature: 19.5,
            target_temperature: f32::NAN,
            swing_mode: ClimateSwingMode::ClimateSwingVertical.into(),
            ..Default::default()
        };
        let modes = Modes {
            fan: vec![],
            swing: vec!["off".into(), "vertical".into()],
            two_point: false,
        };
        let r = report_from(&s, &modes);
        assert_eq!(r.mode, Some(Mode::Auto));
        assert_eq!(r.action, Some(Action::Heating));
        assert_eq!((r.current, r.target), (Some(19.5), None));
        assert_eq!(r.swing.as_deref(), Some("vertical"));
        assert_eq!(r.fan, None); // fan_mode 0 ("on") isn't a mode this board lists
    }
}
