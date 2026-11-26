//! Time-sharing arbiter for the head units on one outdoor unit.
//!
//! A multi-split outdoor unit runs either heating or cooling, never both. Head units left in
//! their own AUTO mode pick heat or cool independently, so the outdoor unit ends up serving
//! whichever head unit asked first and parks the others in standby, or flips back and forth.
//! The arbiter decides one side (heat or cool) for the whole outdoor unit, turns every AUTO
//! request into an explicit HEAT or COOL command on that side, and time-shares the outdoor unit
//! when rooms disagree.
//!
//! This module does no I/O and reads no clock. The service calls [`GroupArbiter::step`] with the
//! current time, the user requests and the head-unit reports, and sends the returned commands.
//!
//! Rules, in order:
//!
//! 1. **Demand.** A unit calls for heat or cool when its room is `start_offset` past its
//!    setpoint (the AUTO low setpoint for heat, high for cool), and keeps calling until the room
//!    gets back to the setpoint. Its demand is how far past the setpoint it is. DRY always
//!    calls for cool with weight `dry_weight`. OFF and FAN_ONLY never call.
//! 2. **Choosing a side.** The outdoor unit stays on its current side while that side has
//!    demand. If only the other side has demand, it switches once the current side has been
//!    satisfied for `idle_switch_delay`. If both sides have demand, it switches after `min_run`
//!    if the other side's demand is at least `demand_ratio` times larger, and in any case after
//!    `max_slice`. That alternation is the time-share. With no demand anywhere it stays put.
//! 3. **Changeover.** Before switching sides every heat/cool/dry unit is parked (see
//!    `idle_action`) for `changeover_settle`, so the outdoor unit never sees mixed requests.
//! 4. **Commands.** On the chosen side, AUTO units run that side's setpoint (HEAT at `low` or
//!    COOL at `high`); explicit HEAT/COOL/DRY units on the matching side run as asked; explicit
//!    units on the wrong side are parked until their turn.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{Action, Mode, Side, UnitCommand, UnitReport, UnitRequest};

/// What a unit that can't run right now is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdleAction {
    #[default]
    Off,
    Fan,
}

/// Tuning for one outdoor unit. Times in seconds, temperatures in degrees Celsius.
#[derive(Clone, Debug, PartialEq)]
pub struct ArbiterConfig {
    pub start_offset: f64,
    pub min_run: f64,
    pub max_slice: f64,
    pub idle_switch_delay: f64,
    pub demand_ratio: f64,
    pub changeover_settle: f64,
    pub dry_weight: f64,
    pub idle_action: IdleAction,
}

impl Default for ArbiterConfig {
    fn default() -> Self {
        ArbiterConfig {
            start_offset: 0.5,
            min_run: 15.0 * 60.0,
            max_slice: 40.0 * 60.0,
            idle_switch_delay: 3.0 * 60.0,
            demand_ratio: 2.0,
            changeover_settle: 60.0,
            dry_weight: 0.5,
            idle_action: IdleAction::Off,
        }
    }
}

impl ArbiterConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_slice < self.min_run {
            return Err("max_slice must be at least min_run".into());
        }
        if self.start_offset < 0.0 || self.demand_ratio < 1.0 {
            return Err("start_offset must be >= 0 and demand_ratio >= 1".into());
        }
        if self.changeover_settle < 0.0 || self.idle_switch_delay < 0.0 || self.dry_weight <= 0.0 {
            return Err(
                "changeover_settle and idle_switch_delay must be >= 0, dry_weight > 0".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Demand {
    pub side: Option<Side>,
    pub amount: f64,
}

impl Demand {
    const NONE: Demand = Demand {
        side: None,
        amount: 0.0,
    };

    fn of(side: Side, amount: f64) -> Demand {
        Demand {
            side: Some(side),
            amount,
        }
    }
}

/// Per-unit result of a step, for frontends.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitStatus {
    /// `None`: leave the head unit alone (no side decided yet).
    pub command: Option<UnitCommand>,
    pub action: Action,
    pub demand: Demand,
    /// Calling, but the outdoor unit is on the other side or changing over.
    pub waiting: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepResult {
    pub side: Option<Side>,
    /// Set while every unit is parked for a changeover.
    pub changing_to: Option<Side>,
    /// When the current side (or changeover) began.
    pub since: Option<f64>,
    pub heat_demand: f64,
    pub cool_demand: f64,
    pub units: BTreeMap<String, UnitStatus>,
}

/// Arbiter state for one outdoor unit and its head units.
#[derive(Clone, Debug, Default)]
pub struct GroupArbiter {
    pub config: ArbiterConfig,
    pub side: Option<Side>,
    pub since: Option<f64>,
    pub changing_to: Option<Side>,
    change_until: Option<f64>,
    last_heat_demand: Option<f64>,
    last_cool_demand: Option<f64>,
    calling: BTreeMap<String, Side>,
}

fn demand_of(total: &[f64; 2], side: Side) -> f64 {
    total[side as usize]
}

impl GroupArbiter {
    pub fn new(config: ArbiterConfig) -> Self {
        GroupArbiter {
            config,
            ..Default::default()
        }
    }

    fn last_demand(&self, side: Side) -> Option<f64> {
        match side {
            Side::Heat => self.last_heat_demand,
            Side::Cool => self.last_cool_demand,
        }
    }

    fn set_last_demand(&mut self, side: Side, t: f64) {
        match side {
            Side::Heat => self.last_heat_demand = Some(t),
            Side::Cool => self.last_cool_demand = Some(t),
        }
    }

    // -- demand ------------------------------------------------------------------------------

    fn demand(&mut self, uid: &str, req: &UnitRequest, rep: Option<&UnitReport>) -> Demand {
        let cfg = &self.config;
        let temp = rep.filter(|r| r.available).and_then(|r| r.current);

        match req.mode {
            Mode::Off | Mode::FanOnly => {
                self.calling.remove(uid);
                return Demand::NONE;
            }
            Mode::Dry => {
                self.calling.insert(uid.to_string(), Side::Cool);
                return Demand::of(Side::Cool, cfg.dry_weight);
            }
            _ => {}
        }
        let Some(temp) = temp else {
            // No room temperature: explicit modes still count as a small call; AUTO can't decide.
            self.calling.remove(uid);
            return match req.mode {
                Mode::Heat => Demand::of(Side::Heat, cfg.dry_weight),
                Mode::Cool => Demand::of(Side::Cool, cfg.dry_weight),
                _ => Demand::NONE,
            };
        };

        let heat_sp = if req.mode == Mode::Heat {
            req.target
        } else {
            req.low
        };
        let cool_sp = if req.mode == Mode::Cool {
            req.target
        } else {
            req.high
        };
        let wants_heat = matches!(req.mode, Mode::Heat | Mode::Auto);
        let wants_cool = matches!(req.mode, Mode::Cool | Mode::Auto);

        let mut calling = self.calling.get(uid).copied();
        if calling == Some(Side::Heat) && !(wants_heat && temp < heat_sp) {
            calling = None;
        }
        if calling == Some(Side::Cool) && !(wants_cool && temp > cool_sp) {
            calling = None;
        }
        if calling.is_none() {
            if wants_heat && temp <= heat_sp - cfg.start_offset {
                calling = Some(Side::Heat);
            } else if wants_cool && temp >= cool_sp + cfg.start_offset {
                calling = Some(Side::Cool);
            }
        }
        match calling {
            Some(s) => self.calling.insert(uid.to_string(), s),
            None => self.calling.remove(uid),
        };
        match calling {
            Some(Side::Heat) => Demand::of(Side::Heat, (heat_sp - temp).max(0.1)),
            Some(Side::Cool) => Demand::of(Side::Cool, (temp - cool_sp).max(0.1)),
            None => Demand::NONE,
        }
    }

    // -- side selection ----------------------------------------------------------------------

    fn initial_side(
        requests: &BTreeMap<String, UnitRequest>,
        reports: &BTreeMap<String, UnitReport>,
        total: &[f64; 2],
    ) -> Option<Side> {
        // Follow what the outdoor unit is already doing, so starting autobot doesn't flip it.
        let (mut heating, mut cooling) = (0, 0);
        for rep in reports.values().filter(|r| r.available) {
            let running = rep.action != Some(Action::Idle);
            if rep.action == Some(Action::Heating) || (running && rep.mode == Some(Mode::Heat)) {
                heating += 1;
            } else if matches!(rep.action, Some(Action::Cooling | Action::Drying))
                || (running && matches!(rep.mode, Some(Mode::Cool | Mode::Dry)))
            {
                cooling += 1;
            }
        }
        if heating != cooling {
            return Some(if heating > cooling {
                Side::Heat
            } else {
                Side::Cool
            });
        }
        let (h, c) = (demand_of(total, Side::Heat), demand_of(total, Side::Cool));
        if h > 0.0 || c > 0.0 {
            return Some(if h >= c { Side::Heat } else { Side::Cool });
        }
        // Nobody calling: lean towards whichever setpoint the rooms are nearer to.
        let (mut lean, mut informed) = (0.0, false);
        for (uid, req) in requests {
            let temp = reports
                .get(uid)
                .filter(|r| r.available)
                .and_then(|r| r.current);
            match req.mode {
                Mode::Heat => (lean, informed) = (lean - 1.0, true),
                Mode::Cool | Mode::Dry => (lean, informed) = (lean + 1.0, true),
                Mode::Auto => {
                    if let Some(t) = temp {
                        (lean, informed) = (lean + t - (req.low + req.high) / 2.0, true);
                    }
                }
                Mode::Off | Mode::FanOnly => {}
            }
        }
        informed.then_some(if lean > 0.0 { Side::Cool } else { Side::Heat })
    }

    fn should_switch(&self, now: f64, total: &[f64; 2]) -> bool {
        let (side, since) = (self.side.expect("side"), self.since.expect("since"));
        let cfg = &self.config;
        let (cur, oth) = (demand_of(total, side), demand_of(total, side.other()));
        if oth <= 0.0 {
            return false;
        }
        if cur <= 0.0 {
            return now - self.last_demand(side).unwrap_or(since) >= cfg.idle_switch_delay;
        }
        let elapsed = now - since;
        elapsed >= cfg.min_run && (elapsed >= cfg.max_slice || oth >= cfg.demand_ratio * cur)
    }

    // -- commands ----------------------------------------------------------------------------

    fn park(&self, req: &UnitRequest) -> UnitCommand {
        let mode = match self.config.idle_action {
            IdleAction::Fan => Mode::FanOnly,
            IdleAction::Off => Mode::Off,
        };
        UnitCommand {
            mode,
            target: None,
            fan: req.fan.clone(),
            swing: req.swing.clone(),
        }
    }

    fn command(&self, req: &UnitRequest) -> Option<UnitCommand> {
        let with = |mode, target| UnitCommand {
            mode,
            target,
            fan: req.fan.clone(),
            swing: req.swing.clone(),
        };
        match req.mode {
            Mode::Off => return Some(UnitCommand::new(Mode::Off, None)),
            Mode::FanOnly => return Some(with(Mode::FanOnly, None)),
            _ => {}
        }
        let side = self.side?;
        if self.changing_to.is_some() {
            return Some(self.park(req));
        }
        Some(match (req.mode, side) {
            (Mode::Auto, Side::Heat) => with(Mode::Heat, Some(req.low)),
            (Mode::Auto, Side::Cool) => with(Mode::Cool, Some(req.high)),
            (Mode::Heat, Side::Heat) | (Mode::Cool | Mode::Dry, Side::Cool) => {
                with(req.mode, Some(req.target))
            }
            _ => self.park(req),
        })
    }

    fn action(req: &UnitRequest, cmd: Option<&UnitCommand>, rep: Option<&UnitReport>) -> Action {
        if req.mode == Mode::Off {
            return Action::Off;
        }
        if let Some(cmd) = cmd
            && matches!(cmd.mode, Mode::Off | Mode::FanOnly)
            && cmd.mode != req.mode
        {
            return Action::Idle; // parked
        }
        rep.filter(|r| r.available)
            .and_then(|r| r.action)
            .unwrap_or(Action::Idle)
    }

    // -- main entry --------------------------------------------------------------------------

    pub fn step(
        &mut self,
        now: f64,
        requests: &BTreeMap<String, UnitRequest>,
        reports: &BTreeMap<String, UnitReport>,
    ) -> StepResult {
        self.calling.retain(|uid, _| requests.contains_key(uid));
        let demands: BTreeMap<&String, Demand> = requests
            .iter()
            .map(|(uid, req)| (uid, self.demand(uid, req, reports.get(uid))))
            .collect();
        let mut total = [0.0f64; 2];
        for d in demands.values() {
            if let Some(s) = d.side {
                total[s as usize] += d.amount;
            }
        }
        for s in [Side::Heat, Side::Cool] {
            if demand_of(&total, s) > 0.0 {
                self.set_last_demand(s, now);
            }
        }

        if let Some(target) = self.changing_to {
            if now >= self.change_until.expect("change_until") {
                self.side = Some(target);
                self.since = Some(now);
                self.changing_to = None;
                self.change_until = None;
                self.set_last_demand(target, now);
            }
        } else if self.side.is_none() {
            if let Some(side) = Self::initial_side(requests, reports, &total) {
                self.side = Some(side);
                self.since = Some(now);
                self.set_last_demand(side, now);
            }
        } else if self.should_switch(now, &total) {
            let target = self.side.expect("side").other();
            if self.config.changeover_settle > 0.0 {
                self.changing_to = Some(target);
                self.change_until = Some(now + self.config.changeover_settle);
            } else {
                self.side = Some(target);
                self.set_last_demand(target, now);
            }
            self.since = Some(now);
        }

        let units = requests
            .iter()
            .map(|(uid, req)| {
                let command = self.command(req);
                let demand = demands[uid];
                // An AUTO unit calling for the other side is waiting too, though its command is valid.
                let waiting = demand.side.is_some()
                    && (self.changing_to.is_some() || demand.side != self.side);
                let action = if waiting {
                    Action::Idle
                } else {
                    Self::action(req, command.as_ref(), reports.get(uid))
                };
                (
                    uid.clone(),
                    UnitStatus {
                        command,
                        action,
                        demand,
                        waiting,
                    },
                )
            })
            .collect();

        StepResult {
            side: self.side,
            changing_to: self.changing_to,
            since: self.since,
            heat_demand: demand_of(&total, Side::Heat),
            cool_demand: demand_of(&total, Side::Cool),
            units,
        }
    }
}

/// Turn a change made at the head unit (IR remote, MELCloud, ...) into a new user request.
///
/// Returns the new request, or `None` if the report is consistent with what autobot last sent.
/// The caller decides when to call this: only after the head unit has had time to apply
/// autobot's own command, so its echo isn't mistaken for a user change.
pub fn adopt_external(
    req: &UnitRequest,
    sent: Option<&UnitCommand>,
    rep: &UnitReport,
    deadband: f64,
) -> Option<UnitRequest> {
    let sent = sent?;
    let mode = rep.mode.filter(|_| rep.available)?;
    let same_target = match (sent.target, rep.target) {
        (Some(s), Some(r)) => (r - s).abs() < 0.25,
        _ => true,
    };
    if mode == sent.mode && same_target {
        return None;
    }
    let target = rep.target.unwrap_or(req.target);
    if mode == Mode::Auto {
        return Some(UnitRequest {
            mode: Mode::Auto,
            low: target - deadband / 2.0,
            high: target + deadband / 2.0,
            ..req.clone()
        });
    }
    if mode == sent.mode && req.mode == Mode::Auto && matches!(sent.mode, Mode::Heat | Mode::Cool) {
        // Setpoint nudged at the remote while autobot was running this AUTO unit on one side.
        return Some(if sent.mode == Mode::Heat {
            UnitRequest {
                low: target,
                high: req.high.max(target + deadband),
                ..req.clone()
            }
        } else {
            UnitRequest {
                high: target,
                low: req.low.min(target - deadband),
                ..req.clone()
            }
        });
    }
    if mode == sent.mode && req.mode != mode {
        return None; // parked unit that is where autobot put it; only the setpoint moved
    }
    Some(match mode {
        Mode::Heat | Mode::Cool | Mode::Dry => UnitRequest {
            mode,
            target,
            ..req.clone()
        },
        _ => UnitRequest {
            mode,
            ..req.clone()
        },
    })
}
