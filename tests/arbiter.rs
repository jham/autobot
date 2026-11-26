use std::collections::BTreeMap;

use autobot::arbiter::{ArbiterConfig, GroupArbiter, IdleAction, adopt_external};
use autobot::model::{Action, Mode, Side, UnitCommand, UnitReport, UnitRequest};

const MIN: f64 = 60.0;

fn auto() -> UnitRequest {
    UnitRequest::auto(20.0, 24.0)
}

fn rep(t: f64) -> UnitReport {
    UnitReport::at(t)
}

fn running(t: f64, mode: Mode, action: Action) -> UnitReport {
    UnitReport {
        mode: Some(mode),
        action: Some(action),
        ..UnitReport::at(t)
    }
}

fn cfg() -> ArbiterConfig {
    ArbiterConfig {
        changeover_settle: 0.0,
        ..Default::default()
    }
}

fn map<T: Clone>(items: &[(&str, T)]) -> BTreeMap<String, T> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn cmd(mode: Mode, target: f64) -> Option<UnitCommand> {
    Some(UnitCommand::new(mode, Some(target)))
}

#[test]
fn auto_unit_becomes_explicit_heat_on_heat_side() {
    let mut arb = GroupArbiter::new(cfg());
    let r = arb.step(0.0, &map(&[("a", auto())]), &map(&[("a", rep(18.0))]));
    assert_eq!(r.side, Some(Side::Heat));
    assert_eq!(r.units["a"].command, cmd(Mode::Heat, 20.0));
    assert!(!r.units["a"].waiting);
}

#[test]
fn auto_unit_on_cool_side_uses_high_setpoint() {
    let mut arb = GroupArbiter::new(cfg());
    let r = arb.step(0.0, &map(&[("a", auto())]), &map(&[("a", rep(26.0))]));
    assert_eq!(r.side, Some(Side::Cool));
    assert_eq!(r.units["a"].command, cmd(Mode::Cool, 24.0));
}

#[test]
fn all_auto_units_share_one_side() {
    let mut arb = GroupArbiter::new(cfg());
    let reqs = map(&[("warm", auto()), ("cold", auto())]);
    let r = arb.step(
        0.0,
        &reqs,
        &map(&[("warm", rep(26.0)), ("cold", rep(18.5))]),
    );
    // Heat demand 1.5 vs cool 2.0: cool wins the first turn; the cold room waits.
    assert_eq!(r.side, Some(Side::Cool));
    assert_eq!(r.units["warm"].command, cmd(Mode::Cool, 24.0));
    assert_eq!(r.units["cold"].command, cmd(Mode::Cool, 24.0));
    assert!(r.units["cold"].waiting);
    assert_eq!(r.units["cold"].action, Action::Idle);
}

#[test]
fn starting_follows_what_the_outdoor_unit_is_already_doing() {
    let mut arb = GroupArbiter::new(cfg());
    let r = arb.step(
        0.0,
        &map(&[("a", auto()), ("b", auto())]),
        &map(&[
            ("a", running(22.0, Mode::Heat, Action::Heating)),
            ("b", rep(26.0)),
        ]),
    );
    assert_eq!(r.side, Some(Side::Heat));
}

#[test]
fn time_share_switches_after_max_slice_and_back() {
    let mut arb = GroupArbiter::new(cfg());
    let reqs = map(&[("warm", auto()), ("cold", auto())]);
    let reps = map(&[("warm", rep(25.0)), ("cold", rep(18.0))]); // cool 1.0, heat 2.0
    assert_eq!(arb.step(0.0, &reqs, &reps).side, Some(Side::Heat));
    // Heat demand is larger, so cool only gets its turn at max_slice.
    assert_eq!(arb.step(39.0 * MIN, &reqs, &reps).side, Some(Side::Heat));
    assert_eq!(arb.step(40.0 * MIN, &reqs, &reps).side, Some(Side::Cool));
    // The heat side calls 2x harder: back after min_run.
    assert_eq!(arb.step(54.0 * MIN, &reqs, &reps).side, Some(Side::Cool));
    assert_eq!(arb.step(55.0 * MIN, &reqs, &reps).side, Some(Side::Heat));
}

#[test]
fn much_bigger_demand_takes_over_after_min_run() {
    let mut arb = GroupArbiter::new(cfg());
    let reqs = map(&[("a", auto()), ("b", auto())]);
    let reps = map(&[("a", rep(25.0)), ("b", rep(19.4))]); // cool 1.0 vs heat 0.6
    assert_eq!(arb.step(0.0, &reqs, &reps).side, Some(Side::Cool));
    let reps = map(&[("a", rep(24.4)), ("b", rep(17.0))]); // cool 0.4 (still calling) vs heat 3.0
    assert_eq!(arb.step(14.0 * MIN, &reqs, &reps).side, Some(Side::Cool));
    assert_eq!(arb.step(15.0 * MIN, &reqs, &reps).side, Some(Side::Heat));
}

#[test]
fn satisfied_side_hands_over_after_idle_delay() {
    let mut arb = GroupArbiter::new(cfg());
    let reqs = map(&[("a", auto()), ("b", auto())]);
    assert_eq!(
        arb.step(0.0, &reqs, &map(&[("a", rep(25.0)), ("b", rep(21.0))]))
            .side,
        Some(Side::Cool)
    );
    let reps = map(&[("a", rep(23.5)), ("b", rep(19.0))]); // a satisfied, b now calls for heat
    assert_eq!(arb.step(1.0 * MIN, &reqs, &reps).side, Some(Side::Cool));
    assert_eq!(arb.step(2.9 * MIN, &reqs, &reps).side, Some(Side::Cool));
    assert_eq!(arb.step(3.0 * MIN, &reqs, &reps).side, Some(Side::Heat)); // cool last called at t=0
}

#[test]
fn no_demand_means_no_changeover() {
    let mut arb = GroupArbiter::new(cfg());
    let reqs = map(&[("a", auto())]);
    assert_eq!(
        arb.step(0.0, &reqs, &map(&[("a", rep(26.0))])).side,
        Some(Side::Cool)
    );
    assert_eq!(
        arb.step(120.0 * MIN, &reqs, &map(&[("a", rep(21.0))])).side,
        Some(Side::Cool)
    );
}

#[test]
fn hysteresis_keeps_calling_until_setpoint() {
    let mut arb = GroupArbiter::new(cfg());
    let reqs = map(&[("a", auto())]);
    let side_at = |arb: &mut GroupArbiter, t: f64, temp: f64| {
        arb.step(t, &reqs, &map(&[("a", rep(temp))])).units["a"]
            .demand
            .side
    };
    assert_eq!(side_at(&mut arb, 0.0, 19.7), None); // within start_offset
    assert_eq!(side_at(&mut arb, 1.0, 19.5), Some(Side::Heat));
    assert_eq!(side_at(&mut arb, 2.0, 19.9), Some(Side::Heat));
    assert_eq!(side_at(&mut arb, 3.0, 20.0), None);
}

#[test]
fn explicit_unit_on_wrong_side_is_parked() {
    let mut arb = GroupArbiter::new(ArbiterConfig {
        idle_action: IdleAction::Fan,
        ..cfg()
    });
    let cool = UnitRequest {
        fan: Some("low".into()),
        ..UnitRequest::mode(Mode::Cool, 23.0)
    };
    let reqs = map(&[("a", cool), ("b", UnitRequest::mode(Mode::Heat, 22.0))]);
    let r = arb.step(0.0, &reqs, &map(&[("a", rep(27.0)), ("b", rep(21.0))]));
    assert_eq!(r.side, Some(Side::Cool));
    assert_eq!(
        r.units["a"].command,
        Some(UnitCommand {
            fan: Some("low".into()),
            ..UnitCommand::new(Mode::Cool, Some(23.0))
        })
    );
    assert_eq!(r.units["b"].command.as_ref().unwrap().mode, Mode::FanOnly);
    assert!(r.units["b"].waiting);
}

#[test]
fn dry_counts_as_cooling() {
    let mut arb = GroupArbiter::new(cfg());
    let r = arb.step(
        0.0,
        &map(&[("a", UnitRequest::mode(Mode::Dry, 24.0))]),
        &map(&[("a", rep(22.0))]),
    );
    assert_eq!(r.side, Some(Side::Cool));
    assert_eq!(r.units["a"].command, cmd(Mode::Dry, 24.0));
}

#[test]
fn off_and_fan_pass_through_without_a_side() {
    let mut arb = GroupArbiter::new(cfg());
    let fan = UnitRequest {
        fan: Some("high".into()),
        ..UnitRequest::mode(Mode::FanOnly, 21.0)
    };
    let r = arb.step(
        0.0,
        &map(&[("a", UnitRequest::default()), ("b", fan)]),
        &BTreeMap::new(),
    );
    assert_eq!(r.side, None);
    assert_eq!(
        r.units["a"].command,
        Some(UnitCommand::new(Mode::Off, None))
    );
    assert_eq!(
        r.units["b"].command,
        Some(UnitCommand {
            fan: Some("high".into()),
            ..UnitCommand::new(Mode::FanOnly, None)
        })
    );
}

#[test]
fn auto_without_temperature_waits_for_data() {
    let mut arb = GroupArbiter::new(cfg());
    let r = arb.step(
        0.0,
        &map(&[("a", auto())]),
        &map(&[("a", UnitReport::default())]),
    );
    assert_eq!(r.side, None);
    assert_eq!(r.units["a"].command, None);
}

#[test]
fn changeover_parks_everything_first() {
    let mut arb = GroupArbiter::new(ArbiterConfig {
        changeover_settle: 60.0,
        ..cfg()
    });
    let reqs = map(&[("a", auto()), ("b", auto())]);
    assert_eq!(
        arb.step(0.0, &reqs, &map(&[("a", rep(25.0)), ("b", rep(21.0))]))
            .side,
        Some(Side::Cool)
    );
    let reps = map(&[("a", rep(23.0)), ("b", rep(18.0))]);
    let r = arb.step(10.0 * MIN, &reqs, &reps);
    assert_eq!(
        (r.side, r.changing_to),
        (Some(Side::Cool), Some(Side::Heat))
    );
    assert!(
        r.units
            .values()
            .all(|u| u.command.as_ref().unwrap().mode == Mode::Off)
    );
    assert!(r.units["b"].waiting);
    assert_eq!(
        arb.step(10.0 * MIN + 30.0, &reqs, &reps).changing_to,
        Some(Side::Heat)
    );
    let r = arb.step(11.0 * MIN, &reqs, &reps);
    assert_eq!((r.side, r.changing_to), (Some(Side::Heat), None));
    assert_eq!(r.units["b"].command, cmd(Mode::Heat, 20.0));
}

fn reported(mode: Mode, target: f64) -> UnitReport {
    UnitReport {
        available: true,
        mode: Some(mode),
        target: Some(target),
        ..Default::default()
    }
}

#[test]
fn adopt_external_mode_change() {
    let sent = UnitCommand::new(Mode::Heat, Some(20.0));
    assert_eq!(
        adopt_external(&auto(), Some(&sent), &reported(Mode::Heat, 20.0), 2.0),
        None
    );
    let new = adopt_external(&auto(), Some(&sent), &reported(Mode::Cool, 25.0), 2.0).unwrap();
    assert_eq!((new.mode, new.target), (Mode::Cool, 25.0));
    let new = adopt_external(&auto(), Some(&sent), &reported(Mode::Auto, 22.0), 2.0).unwrap();
    assert_eq!((new.mode, new.low, new.high), (Mode::Auto, 21.0, 23.0));
}

#[test]
fn adopt_external_setpoint_nudge_on_auto_unit() {
    let heat = UnitCommand::new(Mode::Heat, Some(20.0));
    let new = adopt_external(&auto(), Some(&heat), &reported(Mode::Heat, 21.0), 2.0).unwrap();
    assert_eq!((new.mode, new.low, new.high), (Mode::Auto, 21.0, 24.0));
    let cool = UnitCommand::new(Mode::Cool, Some(24.0));
    let new = adopt_external(&auto(), Some(&cool), &reported(Mode::Cool, 23.0), 2.0).unwrap();
    assert_eq!((new.low, new.high), (20.0, 23.0));
}

#[test]
fn adopt_external_leaves_parked_units_alone() {
    // A HEAT unit parked OFF during a cool turn: OFF at the head unit is what autobot sent.
    let sent = UnitCommand::new(Mode::Off, None);
    let req = UnitRequest::mode(Mode::Heat, 22.0);
    assert_eq!(
        adopt_external(&req, Some(&sent), &reported(Mode::Off, 22.0), 2.0),
        None
    );
}
