use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use autobot::config::Config;
use autobot::model::{Action, Extras, Mode, Select, UnitCommand, UnitReport, Vane};
use autobot::service::{Backend, Change, Frontend, OutdoorState, Service, VirtualState};

#[derive(Default)]
struct FakeState {
    report: UnitReport,
    sent: Vec<UnitCommand>,
    extras: Extras,
    vanes_set: Vec<(Vane, String)>,
}

/// A head unit whose report the test sets and whose commands the test reads back.
#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeState>>);

impl Fake {
    fn set(&self, r: UnitReport) {
        self.0.lock().unwrap().report = r;
    }
    fn sent(&self) -> Vec<UnitCommand> {
        self.0.lock().unwrap().sent.clone()
    }
    fn set_extras(&self, x: Extras) {
        self.0.lock().unwrap().extras = x;
    }
    fn vanes_set(&self) -> Vec<(Vane, String)> {
        self.0.lock().unwrap().vanes_set.clone()
    }
}

impl Backend for Fake {
    fn report(&self) -> UnitReport {
        self.0.lock().unwrap().report.clone()
    }
    fn fan_modes(&self) -> Vec<String> {
        vec!["auto".into(), "low".into()]
    }
    fn swing_modes(&self) -> Vec<String> {
        vec!["off".into(), "vertical".into()]
    }
    fn send(&mut self, cmd: &UnitCommand) -> bool {
        self.0.lock().unwrap().sent.push(cmd.clone());
        true
    }
    fn extras(&self) -> Extras {
        self.0.lock().unwrap().extras.clone()
    }
    fn set_vane(&mut self, vane: Vane, option: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .vanes_set
            .push((vane, option.to_string()));
        true
    }
}

#[derive(Clone, Default)]
struct Shown(
    Arc<Mutex<BTreeMap<String, VirtualState>>>,
    Arc<Mutex<Option<f64>>>,
);

impl Frontend for Shown {
    fn publish_unit(&mut self, uid: &str, s: &VirtualState) {
        self.0.lock().unwrap().insert(uid.into(), s.clone());
    }
    fn publish_outdoor(&mut self, _: &str, o: &OutdoorState) {
        *self.1.lock().unwrap() = o.outside;
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    bedroom: Fake,
    office: Fake,
    shown: Shown,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("autobot.toml");
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.toml"),
        &path,
    )
    .unwrap();
    Rig {
        path,
        _dir: dir,
        bedroom: Fake::default(),
        office: Fake::default(),
        shown: Shown::default(),
    }
}

impl Rig {
    fn service(&self) -> Service {
        let cfg = Config::load(&self.path).unwrap();
        let mut b: BTreeMap<String, Box<dyn Backend>> = BTreeMap::new();
        b.insert("bedroom".into(), Box::new(self.bedroom.clone()));
        b.insert("office".into(), Box::new(self.office.clone()));
        Service::new(cfg, b, vec![Box::new(self.shown.clone())])
    }
}

fn report(mode: Mode, action: Action, current: f64, target: Option<f64>) -> UnitReport {
    UnitReport {
        mode: Some(mode),
        action: Some(action),
        target,
        ..UnitReport::at(current)
    }
}

#[test]
fn example_config_loads() {
    let r = rig();
    let cfg = Config::load(&r.path).unwrap();
    let o = &cfg.outdoor[0];
    assert_eq!(o.units, ["bedroom", "office"]);
    assert_eq!(
        (o.arbiter.min_run, o.arbiter.changeover_settle),
        (15.0 * 60.0, 60.0)
    );
    assert_eq!(
        cfg.state_file,
        r.path.parent().unwrap().join("autobot-state.json")
    );
}

#[test]
fn bad_config_is_rejected() {
    let r = rig();
    let text = std::fs::read_to_string(&r.path).unwrap();
    for (from, to) in [
        // The outdoor unit is implied by nesting, so naming it is an unknown field.
        (
            "host = \"192.168.1.42\"",
            "outdoor = \"upstairs\"\nhost = \"192.168.1.42\"",
        ),
        ("id = \"office\"", "id = \"bedroom\""),
        ("max_slice_min = 40", "max_slice_min = 5"),
        ("demand_ratio = 2.0", "demand_ratio = 2.0\nbogus = 1"),
        ("id = \"office\"", "id = \"Office\""),
    ] {
        assert!(text.contains(from), "{from}");
        std::fs::write(&r.path, text.replace(from, to)).unwrap();
        assert!(Config::load(&r.path).is_err(), "accepted {to:?}");
    }
}

#[test]
fn outdoor_overrides_apply() {
    let r = rig();
    let text = std::fs::read_to_string(&r.path)
        .unwrap()
        .replace("# max_slice_min = 30", "max_slice_min = 30");
    std::fs::write(&r.path, text).unwrap();
    assert_eq!(
        Config::load(&r.path).unwrap().outdoor[0].arbiter.max_slice,
        30.0 * 60.0
    );
}

#[test]
fn json_options_load() {
    let r = rig();
    let json = r#"{"outdoor": [{"id": "up", "max_slice_min": 30,
                   "unit": [{"id": "a", "name": "A", "host": "10.0.0.2", "encryption_key": "k"}]}]}"#;
    let path = r.path.with_file_name("options.json");
    std::fs::write(&path, json).unwrap();
    let cfg = Config::load(&path).unwrap();
    assert_eq!(
        (
            cfg.outdoor[0].arbiter.max_slice,
            cfg.units["a"].port,
            cfg.units["a"].outdoor.as_str()
        ),
        (1800.0, 6053, "up")
    );
    assert!(cfg.mqtt.is_none());
}

#[test]
fn auto_requests_become_one_side_and_persist() {
    let r = rig();
    let mut svc = r.service();
    svc.apply("bedroom", Change::Mode(Mode::Auto));
    svc.apply("office", Change::Mode(Mode::Auto));
    r.bedroom.set(report(Mode::Off, Action::Off, 18.0, None));
    r.office.set(report(Mode::Off, Action::Off, 26.5, None));
    svc.step(0.0);
    assert_eq!(r.bedroom.sent(), [UnitCommand::new(Mode::Cool, Some(24.0))]);
    assert_eq!(r.office.sent(), [UnitCommand::new(Mode::Cool, Some(24.0))]);
    let shown = r.shown.0.lock().unwrap()["bedroom"].clone();
    assert!(shown.waiting && shown.request.mode == Mode::Auto);
    svc.step(1.0); // nothing changed: nothing resent
    assert_eq!(r.office.sent().len(), 1);

    assert_eq!(r.service().requests["office"].mode, Mode::Auto);
}

#[test]
fn setpoints_are_kept_apart() {
    let r = rig();
    let mut svc = r.service();
    svc.apply("bedroom", Change::Mode(Mode::Auto));
    svc.apply("bedroom", Change::Low(23.5));
    assert_eq!(
        (svc.requests["bedroom"].low, svc.requests["bedroom"].high),
        (23.5, 25.5)
    );
    svc.apply("bedroom", Change::High(21.0));
    assert_eq!(
        (svc.requests["bedroom"].low, svc.requests["bedroom"].high),
        (19.0, 21.0)
    );
    svc.apply("bedroom", Change::Low(40.0)); // clamped to max_temp, gap kept
    assert_eq!(
        (svc.requests["bedroom"].low, svc.requests["bedroom"].high),
        (29.0, 31.0)
    );
}

#[test]
fn remote_change_is_adopted() {
    let r = rig();
    let mut svc = r.service();
    svc.apply("bedroom", Change::Mode(Mode::Auto));
    r.bedroom.set(report(Mode::Off, Action::Off, 18.0, None));
    svc.step(0.0);
    assert_eq!(
        r.bedroom.sent().last(),
        Some(&UnitCommand::new(Mode::Heat, Some(20.0)))
    );
    // Someone uses the IR remote to pick COOL at 22 a minute later.
    r.bedroom
        .set(report(Mode::Cool, Action::Idle, 18.0, Some(22.0)));
    svc.step(60.0);
    let req = &svc.requests["bedroom"];
    assert_eq!((req.mode, req.target), (Mode::Cool, 22.0));
}

#[test]
fn unapplied_command_is_resent_when_not_adopting() {
    let r = rig();
    let text = std::fs::read_to_string(&r.path)
        .unwrap()
        .replace("adopt_external = true", "adopt_external = false");
    std::fs::write(&r.path, text).unwrap();
    let mut svc = r.service();
    svc.apply("bedroom", Change::Mode(Mode::Heat));
    r.bedroom.set(report(Mode::Off, Action::Off, 18.0, None));
    svc.step(0.0);
    svc.step(10.0);
    assert_eq!(r.bedroom.sent().len(), 1);
    svc.step(31.0);
    assert_eq!(r.bedroom.sent().len(), 2);
}

#[test]
fn first_start_takes_settings_from_the_head_units() {
    let r = rig();
    let mut svc = r.service();
    svc.step(0.0); // nothing reported yet: nothing sent, nothing switched off
    assert!(r.bedroom.sent().is_empty() && r.office.sent().is_empty());
    r.bedroom.set(UnitReport {
        fan: Some("low".into()),
        ..report(Mode::Auto, Action::Heating, 19.0, Some(21.0))
    });
    r.office
        .set(report(Mode::Heat, Action::Heating, 19.0, Some(22.0)));
    svc.step(1.0);
    let b = &svc.requests["bedroom"];
    assert_eq!(
        (b.mode, b.low, b.high, b.fan.as_deref()),
        (Mode::Auto, 20.0, 22.0, Some("low"))
    );
    assert_eq!(svc.requests["office"].mode, Mode::Heat);
    assert_eq!(
        r.bedroom.sent(),
        [UnitCommand {
            fan: Some("low".into()),
            ..UnitCommand::new(Mode::Heat, Some(20.0))
        }]
    );
    assert_eq!(r.office.sent(), [UnitCommand::new(Mode::Heat, Some(22.0))]);
}

#[test]
fn nonsense_setpoints_from_a_head_unit_are_clamped() {
    let r = rig();
    let mut svc = r.service();
    r.bedroom
        .set(report(Mode::Heat, Action::Idle, 18.0, Some(0.0)));
    svc.step(0.0);
    assert_eq!(svc.requests["bedroom"].target, 16.0);
    svc.apply("bedroom", Change::Mode(Mode::Auto));
    svc.step(1.0);
    r.bedroom
        .set(report(Mode::Heat, Action::Idle, 18.0, Some(0.0)));
    svc.step(60.0); // looks like a setpoint nudge at the remote, to an impossible value
    let b = &svc.requests["bedroom"];
    assert_eq!((b.low, b.high), (16.0, 24.0));
}

#[test]
fn reconnecting_unit_gets_the_command_again_instead_of_being_adopted() {
    let r = rig();
    let mut svc = r.service();
    svc.apply("bedroom", Change::Mode(Mode::Auto));
    r.bedroom.set(report(Mode::Off, Action::Off, 18.0, None));
    svc.step(0.0);
    assert_eq!(r.bedroom.sent().len(), 1);
    r.bedroom.set(UnitReport::default()); // board offline
    svc.step(100.0);
    // Back after a reboot, in some other state.
    r.bedroom
        .set(report(Mode::Cool, Action::Idle, 18.0, Some(25.0)));
    svc.step(200.0);
    assert_eq!(svc.requests["bedroom"].mode, Mode::Auto);
    assert_eq!(r.bedroom.sent().len(), 2);
    assert_eq!(
        r.bedroom.sent()[1],
        UnitCommand::new(Mode::Heat, Some(20.0))
    );
}

#[test]
fn vanes_pass_straight_through() {
    let r = rig();
    let mut svc = r.service();
    r.bedroom
        .set(report(Mode::Heat, Action::Heating, 19.0, Some(21.0)));
    r.office
        .set(report(Mode::Heat, Action::Idle, 21.0, Some(21.0)));
    let vane = Select {
        state: Some("AUTO".into()),
        options: vec!["AUTO".into(), "1".into(), "SWING".into()],
        disabled_by_default: false,
    };
    r.bedroom.set_extras(Extras {
        vanes: [(Vane::Vertical, vane.clone())].into(),
        outside: None,
    });
    svc.step(0.0);
    let before = svc.requests["bedroom"].clone();
    assert_eq!(
        r.shown.0.lock().unwrap()["bedroom"].vanes[&Vane::Vertical],
        vane
    );
    assert!(r.shown.0.lock().unwrap()["office"].vanes.is_empty());

    svc.apply("bedroom", Change::Vane(Vane::Vertical, "SWING".into()));
    assert_eq!(
        r.bedroom.vanes_set(),
        [(Vane::Vertical, "SWING".to_string())]
    );
    // Not part of the request, so not saved and not arbitrated.
    assert_eq!(svc.requests["bedroom"], before);
}

#[test]
fn outside_temperature_comes_from_a_connected_unit() {
    let r = rig();
    let mut svc = r.service();
    r.bedroom
        .set(report(Mode::Heat, Action::Heating, 19.0, Some(21.0)));
    r.office
        .set(report(Mode::Heat, Action::Idle, 21.0, Some(21.0)));
    r.office.set_extras(Extras {
        outside: Some(4.5),
        ..Default::default()
    });
    svc.step(0.0);
    assert_eq!(*r.shown.1.lock().unwrap(), Some(4.5));

    r.office.set(UnitReport::default()); // office drops off the network
    svc.step(10.0);
    assert_eq!(*r.shown.1.lock().unwrap(), None);
}
