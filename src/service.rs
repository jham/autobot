//! Wires head-unit backends, the arbiters and the frontends together.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::arbiter::{GroupArbiter, StepResult, adopt_external};
use crate::config::Config;
use crate::model::{Action, Mode, UnitCommand, UnitReport, UnitRequest};

/// What a frontend shows for one virtual unit.
#[derive(Clone, Debug, PartialEq)]
pub struct VirtualState {
    pub available: bool,
    pub request: UnitRequest,
    pub current: Option<f64>,
    pub action: Action,
    pub waiting: bool,
    pub fan_modes: Vec<String>,
    pub swing_modes: Vec<String>,
}

/// A head unit autobot can read and command.
pub trait Backend: Send {
    fn report(&self) -> UnitReport;
    fn fan_modes(&self) -> Vec<String>;
    fn swing_modes(&self) -> Vec<String>;
    /// Send a command. Returns false if the head unit isn't connected.
    fn send(&mut self, cmd: &UnitCommand) -> bool;
}

/// Something that shows the virtual units to users (Home Assistant, Matter, ...).
pub trait Frontend: Send {
    fn publish_unit(&mut self, uid: &str, state: &VirtualState);
    fn publish_outdoor(&mut self, oid: &str, result: &StepResult);
}

/// A user change to one field of a virtual unit.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Mode(Mode),
    Target(f64),
    Low(f64),
    High(f64),
    Fan(String),
    Swing(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A head unit reported, or a frontend wants everything republished.
    Wake,
    Request(String, Change),
}

struct Sent {
    cmd: UnitCommand,
    at: f64,
}

pub struct Service {
    cfg: Config,
    backends: BTreeMap<String, Box<dyn Backend>>,
    frontends: Vec<Box<dyn Frontend>>,
    arbiters: BTreeMap<String, GroupArbiter>,
    pub requests: BTreeMap<String, UnitRequest>,
    /// Units with no saved setting; they take theirs from the head unit's first report.
    unseeded: BTreeSet<String>,
    sent: BTreeMap<String, Sent>,
}

impl Service {
    pub fn new(
        cfg: Config,
        backends: BTreeMap<String, Box<dyn Backend>>,
        frontends: Vec<Box<dyn Frontend>>,
    ) -> Self {
        let arbiters = cfg
            .outdoor
            .iter()
            .map(|o| (o.id.clone(), GroupArbiter::new(o.arbiter.clone())))
            .collect();
        let mut s = Service {
            cfg,
            backends,
            frontends,
            arbiters,
            requests: BTreeMap::new(),
            unseeded: BTreeSet::new(),
            sent: BTreeMap::new(),
        };
        s.load_requests();
        s
    }

    // -- persistence -------------------------------------------------------------------------

    fn load_requests(&mut self) {
        let saved: BTreeMap<String, serde_json::Value> =
            match std::fs::read_to_string(&self.cfg.state_file) {
                Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                    warn!("ignoring {}: {e}", self.cfg.state_file.display());
                    BTreeMap::new()
                }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(e) => {
                    warn!("ignoring {}: {e}", self.cfg.state_file.display());
                    BTreeMap::new()
                }
            };
        for uid in self.cfg.units.keys() {
            match saved
                .get(uid)
                .map(|v| serde_json::from_value::<UnitRequest>(v.clone()))
            {
                Some(Ok(req)) => {
                    self.requests.insert(uid.clone(), req);
                }
                other => {
                    if let Some(Err(e)) = other {
                        warn!("ignoring saved state for {uid}: {e}");
                    }
                    self.requests.insert(uid.clone(), UnitRequest::default());
                    self.unseeded.insert(uid.clone());
                }
            }
        }
    }

    fn save_requests(&self) {
        let path = &self.cfg.state_file;
        let tmp = path.with_extension("json.tmp");
        let result = serde_json::to_string_pretty(&self.requests)
            .map_err(std::io::Error::other)
            .and_then(|json| std::fs::write(&tmp, json))
            .and_then(|()| std::fs::rename(&tmp, path));
        if let Err(e) = result {
            warn!("saving {}: {e}", path.display());
        }
    }

    // -- inputs ------------------------------------------------------------------------------

    /// Keep every setpoint within `min_temp..=max_temp` and the AUTO setpoints at least
    /// `auto_deadband` apart, moving `high` if `low` was the one just set and `low` otherwise.
    fn normalize(&self, mut req: UnitRequest, moved_low: bool) -> UnitRequest {
        let (lo, hi, band) = (self.cfg.min_temp, self.cfg.max_temp, self.cfg.auto_deadband);
        req.target = req.target.clamp(lo, hi);
        req.low = req.low.clamp(lo, hi);
        req.high = req.high.clamp(lo, hi);
        if req.high - req.low < band {
            if moved_low {
                req.high = (req.low + band).min(hi);
                req.low = req.low.min(req.high - band);
            } else {
                req.low = (req.high - band).max(lo);
                req.high = req.high.max(req.low + band);
            }
        }
        req
    }

    /// Apply a user change from a frontend.
    pub fn apply(&mut self, uid: &str, change: Change) {
        let Some(old) = self.requests.get(uid).cloned() else {
            return;
        };
        self.unseeded.remove(uid);
        let mut req = old.clone();
        let mut moved_low = false;
        match change {
            Change::Mode(m) => req.mode = m,
            Change::Target(t) => req.target = t,
            Change::Low(t) => (req.low, moved_low) = (t, true),
            Change::High(t) => req.high = t,
            Change::Fan(f) => req.fan = Some(f),
            Change::Swing(s) => req.swing = Some(s),
        }
        let req = self.normalize(req, moved_low);
        if req != old {
            info!("{uid}: request {req:?}");
            self.requests.insert(uid.to_string(), req);
            self.save_requests();
        }
    }

    // -- the step ----------------------------------------------------------------------------

    fn seed(&mut self, uid: &str) {
        let rep = self.backends[uid].report();
        let Some(mode) = rep.mode.filter(|_| rep.available) else {
            return;
        };
        let req = &self.requests[uid];
        let t = rep.target.unwrap_or(req.target);
        let half = self.cfg.auto_deadband / 2.0;
        let mut new = if mode == Mode::Auto {
            UnitRequest {
                mode,
                low: t - half,
                high: t + half,
                ..req.clone()
            }
        } else {
            UnitRequest {
                mode,
                target: t,
                ..req.clone()
            }
        };
        new.fan = rep.fan.clone();
        new.swing = rep.swing.clone();
        let new = self.normalize(new, false);
        info!("{uid}: no saved setting; starting from the head unit's: {new:?}");
        self.requests.insert(uid.to_string(), new);
        self.unseeded.remove(uid);
        self.save_requests();
    }

    fn adopt(&mut self, uid: &str, now: f64) {
        let Some(sent) = self.sent.get(uid) else {
            return;
        };
        let rep = self.backends[uid].report();
        if !rep.available || now - sent.at < self.cfg.adopt_after {
            return;
        }
        let req = &self.requests[uid];
        let mut new = adopt_external(req, Some(&sent.cmd), &rep, self.cfg.auto_deadband);
        if new.is_none() && rep.fan.is_some() && sent.cmd.fan.is_some() && rep.fan != sent.cmd.fan {
            new = Some(UnitRequest {
                fan: rep.fan.clone(),
                ..req.clone()
            });
        }
        if new.is_none()
            && rep.swing.is_some()
            && sent.cmd.swing.is_some()
            && rep.swing != sent.cmd.swing
        {
            new = Some(UnitRequest {
                swing: rep.swing.clone(),
                ..req.clone()
            });
        }
        let Some(new) = new
            .map(|n| {
                let moved_low = n.low != req.low;
                self.normalize(n, moved_low)
            })
            .filter(|n| n != req)
        else {
            return;
        };
        info!("{uid}: adopting change made at the head unit: {new:?}");
        // Treat the head unit's state as already sent, so it isn't immediately overwritten.
        let echo = UnitCommand {
            mode: rep.mode.unwrap_or(sent.cmd.mode),
            target: rep.target,
            fan: rep.fan,
            swing: rep.swing,
        };
        self.sent
            .insert(uid.to_string(), Sent { cmd: echo, at: now });
        self.requests.insert(uid.to_string(), new);
        self.save_requests();
    }

    fn deliver(&mut self, uid: &str, cmd: &UnitCommand, now: f64) {
        let rep = self.backends[uid].report();
        if !rep.available {
            return;
        }
        let (same, stale) = match self.sent.get(uid) {
            Some(s) if s.cmd == *cmd => (
                true,
                rep.available && now - s.at >= self.cfg.adopt_after && rep.mode != Some(cmd.mode),
            ),
            _ => (false, false),
        };
        if same && !stale {
            return;
        }
        if self.backends.get_mut(uid).expect("backend").send(cmd) {
            info!("{uid}: {} {cmd:?}", if stale { "resend" } else { "send" });
            self.sent.insert(
                uid.to_string(),
                Sent {
                    cmd: cmd.clone(),
                    at: now,
                },
            );
        }
    }

    /// One arbiter pass over every outdoor unit. `now` is seconds on a monotonic clock.
    pub fn step(&mut self, now: f64) {
        let ids: Vec<String> = self.requests.keys().cloned().collect();
        for uid in &ids {
            if self.unseeded.contains(uid) {
                self.seed(uid);
            }
        }
        for uid in &ids {
            // A head unit that went away may come back in any state (a reboot, a power cut);
            // forget what was sent so it gets the current command instead of being adopted.
            if !self.backends[uid].report().available {
                self.sent.remove(uid);
            }
        }
        if self.cfg.adopt_external {
            for uid in &ids {
                self.adopt(uid, now);
            }
        }
        for o in self.cfg.outdoor.clone() {
            let live: Vec<&String> = o
                .units
                .iter()
                .filter(|u| !self.unseeded.contains(*u))
                .collect();
            let reqs: BTreeMap<String, UnitRequest> = live
                .iter()
                .map(|u| ((*u).clone(), self.requests[*u].clone()))
                .collect();
            let reps: BTreeMap<String, UnitReport> = live
                .iter()
                .map(|u| ((*u).clone(), self.backends[*u].report()))
                .collect();
            let arb = self.arbiters.get_mut(&o.id).expect("arbiter");
            let before = (arb.side, arb.changing_to);
            let result = arb.step(now, &reqs, &reps);
            if (result.side, result.changing_to) != before {
                match result.changing_to {
                    Some(to) => info!("{}: changing over to {}", o.id, to.as_str()),
                    None => info!(
                        "{}: now on {}",
                        o.id,
                        result.side.map_or("-", |s| s.as_str())
                    ),
                }
            }
            for (uid, st) in &result.units {
                if let Some(cmd) = &st.command {
                    self.deliver(uid, cmd, now);
                }
                let be = &self.backends[uid];
                let rep = &reps[uid];
                let vs = VirtualState {
                    available: rep.available,
                    request: self.requests[uid].clone(),
                    current: rep.current,
                    action: st.action,
                    waiting: st.waiting,
                    fan_modes: be.fan_modes(),
                    swing_modes: be.swing_modes(),
                };
                for f in &mut self.frontends {
                    f.publish_unit(uid, &vs);
                }
            }
            for f in &mut self.frontends {
                f.publish_outdoor(&o.id, &result);
            }
        }
    }

    /// Run until the event channel closes.
    pub async fn run(mut self, mut events: mpsc::UnboundedReceiver<Event>) {
        let start = Instant::now();
        let tick = Duration::from_secs_f64(self.cfg.tick);
        loop {
            self.step(start.elapsed().as_secs_f64());
            let first = tokio::select! {
                ev = events.recv() => match ev { Some(ev) => Some(ev), None => return },
                () = tokio::time::sleep(tick) => None,
            };
            if let Some(ev) = first {
                // Coalesce a burst of state updates into one step.
                tokio::time::sleep(Duration::from_millis(200)).await;
                for ev in std::iter::once(ev).chain(std::iter::from_fn(|| events.try_recv().ok())) {
                    if let Event::Request(uid, change) = ev {
                        self.apply(&uid, change);
                    }
                }
            }
        }
    }
}
