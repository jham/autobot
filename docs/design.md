# autobot design

## Pieces

```
 Home Assistant ──MQTT──┐                       ┌── esp105 (ESPHome API) ── head unit 1 ─┐
 Matter (planned) ──────┤  frontends  service   │                                         ├─ outdoor unit
                        └──────────── arbiter ──┼── esp105 (ESPHome API) ── head unit 2 ─┘
                                                └── ...
```

- **Frontends** show one virtual unit per head unit and pass user changes to
  `Service::apply()`. A virtual unit holds a `UnitRequest`: mode, a single `target` for
  HEAT, COOL and DRY, `low` and `high` for AUTO, and fan and swing.
- **Backends** keep a `UnitReport` per head unit (mode, action, room temperature, setpoint) and
  carry out `UnitCommand`s. A backend never receives AUTO.
- **The service** runs every `tick` seconds, and also whenever a report or request changes. It
  steps one `GroupArbiter` per outdoor unit. It sends a command only when the command changes,
  or when the head unit still doesn't match it after `adopt_after` seconds.
- **The arbiter** (`src/arbiter.rs`) holds all the policy. It does no I/O and reads no clock, so the
  tests can step it through hours of simulated time.

## Arbiter rules

### Demand

A unit calls for heat when its room is `start_offset` (0.5 °C) below its heat setpoint. It keeps
calling until the room reaches the setpoint. Cooling works the same way above the cool setpoint.
That gap is the hysteresis.

- The heat setpoint is `target` in HEAT and `low` in AUTO.
- The cool setpoint is `target` in COOL and `high` in AUTO.
- A unit's demand is how far the room is past its setpoint.
- DRY always calls for cool, with weight `dry_weight`.
- HEAT or COOL with no room temperature calls with that same weight.
- AUTO with no temperature doesn't call.
- OFF and FAN_ONLY never call. They need no side, so they pass straight through.

### Choosing a side

Demand on each side is summed across the outdoor unit's head units.

| Situation | What happens |
|---|---|
| No side chosen yet (startup) | Follow what the head units are already doing. Otherwise take the side with more demand. Otherwise lean toward the side the rooms are nearer to. |
| Only the current side calls, or nobody calls | Stay. |
| Only the other side calls | Switch once the current side has had no demand for `idle_switch_delay`. |
| Both sides call | After `min_run`, switch if the other side's demand is at least `demand_ratio` × the current side's. Always switch after `max_slice`. |

`max_slice` is what makes this time-sharing: two rooms that both keep calling alternate, each
getting at least `min_run` and at most `max_slice` per turn.

### Changeover

Switching sides puts the arbiter in a changeover state for `changeover_settle`. During it, every
unit that needs a side is set to `idle_action` (off by default, or fan). After that the new side's
commands go out. Without this pause the outdoor unit would briefly see the old and new modes
together, which is the very conflict autobot exists to prevent. The outdoor unit's own compressor
restart protection still applies on top.

### Commands

| Request | On the heat side | On the cool side |
|---|---|---|
| AUTO | HEAT at `low` | COOL at `high` |
| HEAT | HEAT at `target` | parked |
| COOL | parked | COOL at `target` |
| DRY | parked | DRY at `target` |
| FAN_ONLY | FAN_ONLY | FAN_ONLY |
| OFF | OFF | OFF |

An AUTO unit whose room needs the other side keeps the side's setpoint. For example, a cold room
on the cool side holds COOL at its high setpoint. Since the room is far below that setpoint, the
compressor doesn't run, so the unit can't fight the side, and the frontend shows it as waiting.

### Changes made at the head unit

If someone changes a head unit with the IR remote or MELCloud, its report stops matching what
autobot sent. Once `adopt_after` has passed since autobot's last command, `adopt_external()`
turns the change into a new request:

- A new mode becomes the request's mode.
- AUTO picked on the remote becomes AUTO with setpoints `target ± auto_deadband/2`.
- A setpoint nudge on a unit autobot is running in AUTO moves that side's setpoint (`low` or
  `high`).

Set `adopt_external = false` to have autobot put the head unit back instead.

## Home Assistant entities

For each head unit:

- `climate.autobot_<id>`: modes off, heat, cool, heat_cool, dry and fan_only, with fan and
  swing passed through.
- `binary_sensor.autobot_<id>_waiting`: on while the unit is calling but the outdoor unit is on
  the other side.
- `select.autobot_<id>_vertical_vane` and `select.autobot_<id>_horizontal_vane`, for each vane
  select the board has. Options and state are the board's, and changes go straight to the board.
  Vanes don't change which side a unit needs, so the arbiter ignores them and autobot doesn't
  save them. An entity the board marks disabled by default (esp105 does this for the horizontal
  vane) starts disabled in Home Assistant too.

For each outdoor unit:

- A mode sensor: `heat`, `cool`, `changing to heat`, `changing to cool` or `undecided`. Its
  attributes are the heat and cool demand and the list of units waiting.
- An outside temperature sensor, from the board's `Outside air temperature` sensor. Every head
  unit on an outdoor unit reports the same reading, so autobot takes it from the first connected
  unit that has one. The entity appears once a reading arrives, and shows unknown while no unit
  reports one. On esp105 the board's sensor is disabled by default in Home Assistant, but the
  board still reads it, so autobot gets it.

All entities are retained MQTT discovery under `homeassistant/` (configurable). State is under
`autobot/<id>/...`. A will message marks everything unavailable if autobot drops off the broker.
When Home Assistant restarts (`homeassistant/status` = `online`), autobot republishes everything.

## Matter

The plan is to expose the same virtual units to Matter controllers (Apple Home, Google Home,
SmartThings) as a Matter bridge. Each head unit would be a bridged Room Air Conditioner or
Thermostat endpoint:

| Matter (Thermostat cluster) | autobot |
|---|---|
| `SystemMode` Off / Heat / Cool / Auto / Dry / FanOnly | `UnitRequest.mode` |
| `OccupiedHeatingSetpoint`, `OccupiedCoolingSetpoint` | `low`/`target` and `high`/`target` |
| `MinSetpointDeadBand` | `auto_deadband` |
| `LocalTemperature` | `UnitReport.current` |
| `ThermostatRunningState` | the published action (idle while waiting) |
| Fan Control cluster | `fan` |

The plan is a second frontend inside autobot, built on
[rs-matter](https://github.com/project-chip/rs-matter), so a single process serves both Home
Assistant (MQTT) and Matter controllers. The add-on already runs on the host network, which
Matter needs for mDNS and IPv6. Nothing in the arbiter or the service would change; the frontend
implements the same `Frontend` trait and sends `Event::Request`s like the MQTT one.

Home Assistant could also pick autobot up through its own Matter integration. MQTT stays
anyway: Matter has no standard attribute for "waiting for the outdoor unit", and Home Assistant
shows Matter fan control as a separate entity.

A second, separate question is head units that run the esp105 **Matter** firmware (Thread)
instead of the Wi-Fi firmware. Supporting them would mean a backend that controls them as a
Matter controller. The `Backend` trait (`report()`, `send()`, the available fan and swing modes)
is meant to allow that.

## ESPHome API version

autobot uses the `esphome-client` crate with its newest protocol definition, API 1.16, pinned.
ESPHome 2026.11 speaks API 1.19. The handshake logs a version-mismatch warning, but the messages
autobot uses are unchanged, and it worked against a 2026.11 device. When the crate adds a newer
API version, move the `api-1-NN` feature in `Cargo.toml` forward.

ESPHome 2026.11 also leaves `object_id` empty in entity listings, so the optional `climate`
setting matches the entity's name as well ("Heat pump" or "heat_pump").

## Not handled yet

- Arbiter timing state (current side, time on it) is not saved. After a restart autobot follows
  what the head units are doing, which is safe, but the current turn's timer starts over.
- An outdoor unit that autobot doesn't fully control (a head unit without an esp105) is invisible
  to the arbiter. That unit can still claim the outdoor unit for the other side.
- Defrost is reported as an action but doesn't change any decision.
- Climate entities with two setpoints (low/high) aren't driven properly: autobot only sets a
  single target. The esp105's cn105 climate has one setpoint, so this only affects other
  boards.
