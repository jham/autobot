//! Home Assistant frontend: one MQTT climate entity per head unit, via MQTT discovery.
//!
//! Each virtual unit offers off / heat / cool / heat_cool / dry / fan_only. `heat_cool` is
//! autobot's AUTO: a low and a high setpoint, with autobot choosing heat or cool in step with the
//! rest of the outdoor unit. Each outdoor unit also gets a sensor showing which side it is on.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rumqttc::{AsyncClient, Event as MqttEvent, LastWill, MqttOptions, Packet, QoS};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::arbiter::StepResult;
use crate::config::{Config, MqttConfig, OutdoorConfig, UnitConfig};
use crate::model::Mode;
use crate::service::{Change, Event, Frontend, VirtualState};

const HA_MODES: [(Mode, &str); 6] = [
    (Mode::Off, "off"),
    (Mode::Heat, "heat"),
    (Mode::Cool, "cool"),
    (Mode::Auto, "heat_cool"),
    (Mode::Dry, "dry"),
    (Mode::FanOnly, "fan_only"),
];

fn ha_mode(m: Mode) -> &'static str {
    HA_MODES
        .iter()
        .find(|(k, _)| *k == m)
        .map(|(_, v)| *v)
        .expect("every mode has a name")
}

fn mode_from_ha(s: &str) -> Option<Mode> {
    if s == "auto" {
        return Some(Mode::Auto);
    }
    HA_MODES.iter().find(|(_, v)| *v == s).map(|(k, _)| *k)
}

fn num(x: Option<f64>) -> String {
    x.map(|v| format!("{v:.1}")).unwrap_or_default()
}

/// Parse a command topic (`<base>/<unit>/<field>/set`) and payload into a change.
pub fn parse_command(base: &str, topic: &str, payload: &str) -> Option<(String, Change)> {
    let rest = topic.strip_prefix(base)?.strip_prefix('/')?;
    let mut parts = rest.split('/');
    let (uid, what, set) = (parts.next()?, parts.next()?, parts.next()?);
    if set != "set" || parts.next().is_some() {
        return None;
    }
    let payload = payload.trim();
    let change = match what {
        "mode" => Change::Mode(mode_from_ha(payload)?),
        "target" => Change::Target(payload.parse().ok()?),
        "low" => Change::Low(payload.parse().ok()?),
        "high" => Change::High(payload.parse().ok()?),
        "fan" => Change::Fan(payload.to_string()),
        "swing" => Change::Swing(payload.to_string()),
        _ => return None,
    };
    Some((uid.to_string(), change))
}

struct Publisher {
    client: AsyncClient,
    m: MqttConfig,
    outdoor: Vec<OutdoorConfig>,
    units: BTreeMap<String, UnitConfig>,
    min_temp: f64,
    max_temp: f64,
    published: HashMap<String, String>,
    modes: BTreeMap<String, (Vec<String>, Vec<String>)>,
}

impl Publisher {
    fn t(&self, parts: &[&str]) -> String {
        std::iter::once(self.m.base_topic.as_str())
            .chain(parts.iter().copied())
            .collect::<Vec<_>>()
            .join("/")
    }

    fn put(&mut self, topic: String, payload: String) {
        if self.published.get(&topic) == Some(&payload) {
            return;
        }
        // try_publish only queues; if the queue is full the next step tries again.
        match self
            .client
            .try_publish(&topic, QoS::AtLeastOnce, true, payload.clone())
        {
            Ok(()) => {
                self.published.insert(topic, payload);
            }
            Err(e) => warn!("mqtt: queueing {topic}: {e}"),
        }
    }

    fn discover_unit(&mut self, uid: &str) {
        let Some(uc) = self.units.get(uid).cloned() else {
            return;
        };
        let Some(oc) = self.outdoor.iter().find(|o| o.id == uc.outdoor).cloned() else {
            return;
        };
        let (fan_modes, swing_modes) = self.modes.get(uid).cloned().unwrap_or_default();
        let t = |p: &[&str]| self.t(&[&[uid], p].concat());
        let config = json!({
            "name": null,
            "unique_id": format!("autobot_{uid}"),
            "default_entity_id": format!("climate.autobot_{uid}"),
            "device": {
                "identifiers": [format!("autobot_{uid}")],
                "name": uc.name,
                "manufacturer": "Mitsubishi Electric",
                "model": format!("Head unit on {} (via autobot)", oc.name),
                "via_device": format!("autobot_outdoor_{}", oc.id),
            },
            "availability": [{"topic": self.t(&["status"])}, {"topic": t(&["availability"])}],
            "availability_mode": "all",
            "modes": HA_MODES.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
            "mode_state_topic": t(&["mode"]), "mode_command_topic": t(&["mode", "set"]),
            "temperature_state_topic": t(&["target"]), "temperature_command_topic": t(&["target", "set"]),
            "temperature_low_state_topic": t(&["low"]), "temperature_low_command_topic": t(&["low", "set"]),
            "temperature_high_state_topic": t(&["high"]), "temperature_high_command_topic": t(&["high", "set"]),
            "current_temperature_topic": t(&["current"]),
            "action_topic": t(&["action"]),
            "fan_modes": fan_modes, "fan_mode_state_topic": t(&["fan"]), "fan_mode_command_topic": t(&["fan", "set"]),
            "swing_modes": swing_modes, "swing_mode_state_topic": t(&["swing"]), "swing_mode_command_topic": t(&["swing", "set"]),
            "min_temp": self.min_temp, "max_temp": self.max_temp,
            "temp_step": 0.5, "precision": 0.5, "temperature_unit": "C",
            "optimistic": false,
        });
        let waiting = json!({
            "name": "Waiting for outdoor unit",
            "unique_id": format!("autobot_{uid}_waiting"),
            "device": {"identifiers": [format!("autobot_{uid}")]},
            "availability": [{"topic": self.t(&["status"])}],
            "state_topic": t(&["waiting"]),
            "entity_category": "diagnostic",
            "icon": "mdi:timer-sand",
        });
        let p = self.m.discovery_prefix.clone();
        self.put(
            format!("{p}/climate/autobot_{uid}/config"),
            config.to_string(),
        );
        self.put(
            format!("{p}/binary_sensor/autobot_{uid}_waiting/config"),
            waiting.to_string(),
        );
    }

    fn discover_outdoor(&mut self) {
        for o in self.outdoor.clone() {
            let config = json!({
                "name": "Mode",
                "unique_id": format!("autobot_outdoor_{}_mode", o.id),
                "device": {
                    "identifiers": [format!("autobot_outdoor_{}", o.id)],
                    "name": o.name,
                    "manufacturer": "Mitsubishi Electric",
                    "model": "Outdoor unit (via autobot)",
                },
                "availability": [{"topic": self.t(&["status"])}],
                "state_topic": self.t(&["outdoor", &o.id, "state"]),
                "json_attributes_topic": self.t(&["outdoor", &o.id, "attributes"]),
                "icon": "mdi:hvac",
            });
            let p = self.m.discovery_prefix.clone();
            self.put(
                format!("{p}/sensor/autobot_outdoor_{}/config", o.id),
                config.to_string(),
            );
        }
    }

    /// Forget what was published and send discovery again. The service's next step resends state.
    fn republish(&mut self) {
        self.published.clear();
        self.discover_outdoor();
        for uid in self.modes.keys().cloned().collect::<Vec<_>>() {
            self.discover_unit(&uid);
        }
    }
}

/// Handle to the MQTT connection; a background task runs the connection itself.
pub struct MqttHomeAssistant {
    p: Arc<Mutex<Publisher>>,
}

impl MqttHomeAssistant {
    pub fn spawn(cfg: &Config, mqtt: MqttConfig, events: mpsc::UnboundedSender<Event>) -> Self {
        let status = format!("{}/status", mqtt.base_topic);
        let mut opts = MqttOptions::new("autobot", mqtt.host.clone(), mqtt.port);
        opts.set_keep_alive(Duration::from_secs(30));
        opts.set_last_will(LastWill::new(&status, "offline", QoS::AtLeastOnce, true));
        if let Some(user) = &mqtt.username {
            opts.set_credentials(user, mqtt.password.clone().unwrap_or_default());
        }
        let (client, mut eventloop) = AsyncClient::new(opts, 1024);
        let p = Arc::new(Mutex::new(Publisher {
            client: client.clone(),
            m: mqtt.clone(),
            outdoor: cfg.outdoor.clone(),
            units: cfg.units.clone(),
            min_temp: cfg.min_temp,
            max_temp: cfg.max_temp,
            published: HashMap::new(),
            modes: BTreeMap::new(),
        }));
        let shared = p.clone();
        tokio::spawn(async move {
            let ha_status = format!("{}/status", mqtt.discovery_prefix);
            let commands = format!("{}/+/+/set", mqtt.base_topic);
            let mut delay = Duration::from_secs(2);
            loop {
                match eventloop.poll().await {
                    Ok(MqttEvent::Incoming(Packet::ConnAck(_))) => {
                        info!("mqtt: connected to {}:{}", mqtt.host, mqtt.port);
                        delay = Duration::from_secs(2);
                        for topic in [&commands, &ha_status] {
                            if let Err(e) = client.try_subscribe(topic, QoS::AtLeastOnce) {
                                warn!("mqtt: subscribing to {topic}: {e}");
                            }
                        }
                        let mut p = shared.lock().expect("publisher");
                        p.republish();
                        let _ = client.try_publish(&status, QoS::AtLeastOnce, true, "online");
                        drop(p);
                        let _ = events.send(Event::Wake);
                    }
                    Ok(MqttEvent::Incoming(Packet::Publish(msg))) => {
                        let payload = String::from_utf8_lossy(&msg.payload);
                        if msg.topic == ha_status {
                            if payload == "online" {
                                // Home Assistant restarted: send everything again.
                                shared.lock().expect("publisher").republish();
                                let _ = events.send(Event::Wake);
                            }
                        } else if let Some((uid, change)) =
                            parse_command(&mqtt.base_topic, &msg.topic, &payload)
                        {
                            let _ = events.send(Event::Request(uid, change));
                        } else {
                            warn!("mqtt: ignoring {} = {payload:?}", msg.topic);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("mqtt: {e}; retrying in {}s", delay.as_secs());
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(60));
                    }
                }
            }
        });
        MqttHomeAssistant { p }
    }
}

impl Frontend for MqttHomeAssistant {
    fn publish_unit(&mut self, uid: &str, s: &VirtualState) {
        let mut p = self.p.lock().expect("publisher");
        let r = &s.request;
        let t = |field: &str| p.t(&[uid, field]);
        let topics = [
            (
                t("availability"),
                if s.available { "online" } else { "offline" }.to_string(),
            ),
            (t("mode"), ha_mode(r.mode).to_string()),
            (t("target"), num(Some(r.target))),
            (t("low"), num(Some(r.low))),
            (t("high"), num(Some(r.high))),
            (t("current"), num(s.current)),
            (t("action"), s.action.as_str().to_string()),
            (t("fan"), r.fan.clone().unwrap_or_else(|| "auto".into())),
            (t("swing"), r.swing.clone().unwrap_or_else(|| "off".into())),
            (
                t("waiting"),
                if s.waiting { "ON" } else { "OFF" }.to_string(),
            ),
        ];
        let modes = (s.fan_modes.clone(), s.swing_modes.clone());
        if p.modes.get(uid) != Some(&modes) {
            p.modes.insert(uid.to_string(), modes);
            p.discover_unit(uid);
        }
        for (topic, payload) in topics {
            p.put(topic, payload);
        }
    }

    fn publish_outdoor(&mut self, oid: &str, r: &StepResult) {
        let mut p = self.p.lock().expect("publisher");
        let state = match (r.changing_to, r.side) {
            (Some(to), _) => format!("changing to {}", to.as_str()),
            (None, Some(side)) => side.as_str().to_string(),
            (None, None) => "undecided".to_string(),
        };
        let waiting: Vec<&String> = r
            .units
            .iter()
            .filter(|(_, u)| u.waiting)
            .map(|(k, _)| k)
            .collect();
        let attrs = json!({
            "heat_demand": (r.heat_demand * 100.0).round() / 100.0,
            "cool_demand": (r.cool_demand * 100.0).round() / 100.0,
            "waiting": waiting,
        });
        let (st, at) = (
            p.t(&["outdoor", oid, "state"]),
            p.t(&["outdoor", oid, "attributes"]),
        );
        p.put(st, state);
        p.put(at, attrs.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(
            parse_command("autobot", "autobot/office/mode/set", "heat_cool"),
            Some(("office".into(), Change::Mode(Mode::Auto)))
        );
        assert_eq!(
            parse_command("autobot", "autobot/office/low/set", "19.5"),
            Some(("office".into(), Change::Low(19.5)))
        );
        assert_eq!(
            parse_command("autobot", "autobot/office/fan/set", "quiet"),
            Some(("office".into(), Change::Fan("quiet".into())))
        );
        assert_eq!(
            parse_command("autobot", "autobot/office/mode/set", "bogus"),
            None
        );
        assert_eq!(
            parse_command("autobot", "autobot/office/target/set", "warm"),
            None
        );
        assert_eq!(
            parse_command("autobot", "autobot/office/mode", "heat"),
            None
        );
        assert_eq!(
            parse_command("autobot", "other/office/mode/set", "heat"),
            None
        );
    }
}
