# autobot

autobot coordinates AUTO mode across the head units that share one Mitsubishi multi-split
outdoor unit. Each head unit has an [esp105](../esp105) board on its CN105 port.

## The problem

A multi-split outdoor unit can heat or cool, but not both at once. Each head unit in AUTO picks
heat or cool by itself. When two rooms disagree, the outdoor unit runs whichever unit asked first
and puts the other in standby. It can also flip between heat and cool as rooms drift, and nothing
decides which room gets served or for how long.

## What autobot does

autobot connects to every esp105 board and shows Home Assistant one climate entity per head unit
(a 1:1 mapping). Each entity offers `off`, `heat`, `cool`, `heat_cool`, `dry` and `fan_only`.
`heat_cool` is autobot's AUTO mode, with a low and a high setpoint.

For each outdoor unit, autobot:

1. **Picks one side, heat or cool**, based on how far each room is past its setpoint.
2. **Never sends AUTO to a head unit.** On the heat side, an AUTO unit runs HEAT at its low
   setpoint. On the cool side, it runs COOL at its high setpoint. So every head unit on an
   outdoor unit always runs in the same direction.
3. **Takes turns when rooms disagree.** It stays on a side for at least `min_run` (15 min). It
   switches early only if the waiting side calls at least twice as hard. Otherwise it switches
   after `max_slice` (40 min), so neither side waits forever. With no demand anywhere, it stays
   on its current side.
4. **Parks every unit before a changeover.** For `changeover_settle` (60 s) all units are set to
   `idle_action` (off, or fan), so the outdoor unit never sees a mix of heat and cool calls.
5. **Parks units in explicit HEAT, COOL or DRY when the outdoor unit is on the other side.**
   They wait their turn and show as `idle`. A "Waiting for outdoor unit" sensor turns on.
6. **Picks up changes made at the head unit.** If someone uses the IR remote, autobot adopts the
   new setting instead of overriding it (`adopt_external`).

On first start, each unit takes its setting from what its head unit is doing, so starting autobot
changes nothing. Settings are saved to `autobot-state.json`.

The rules are explained in more detail in [docs/design.md](docs/design.md). The code is in
[`src/arbiter.rs`](src/arbiter.rs), which does no I/O and is covered by
[`tests/arbiter.rs`](tests/arbiter.rs).

autobot is a single Rust binary. It runs as a Home Assistant add-on, or on its own anywhere
that can reach the boards and an MQTT broker.

## Layout

| Path | What it is |
|---|---|
| `src/arbiter.rs` | The time-sharing logic. Pure functions of time, requests and reports. |
| `src/model.rs` | Shared data types (modes, requests, reports, commands). |
| `src/service.rs` | The main loop. Saves settings, adopts remote changes, sends commands. |
| `src/backends/esphome.rs` | Talks to esp105 boards running the Wi-Fi firmware (ESPHome native API, via the `esphome-client` crate). |
| `src/frontends/mqtt_ha.rs` | Home Assistant entities over MQTT discovery. |
| `src/config.rs` | Loads `autobot.toml`, or the add-on's `/data/options.json` (same shape). |
| `src/supervisor.rs` | Gets the MQTT broker's details from the Home Assistant Supervisor. |
| `addon/` | The Home Assistant add-on: manifest, Dockerfile, docs. |
| `config.example.toml` | Every option, with comments. |

## As a Home Assistant add-on

1. In Home Assistant, go to **Settings → Add-ons → Add-on Store → ⋮ → Repositories** and add
   this repository's URL.
2. Install **autobot**.
3. Fill in the outdoor units and head units on the **Configuration** tab. The
   **Documentation** tab explains each option.

You need the Mosquitto broker add-on. autobot gets the broker's details from the Supervisor.

The add-on pulls a prebuilt image (`ghcr.io/jham/autobot-{arch}`, for `amd64` and `aarch64`).
CI builds and pushes it when a `v*` tag is pushed. The tag must match the version in
`Cargo.toml` and `addon/config.yaml`.

## On its own

You need Rust 1.88 or newer.

```bash
cp config.example.toml autobot.toml
```

Edit `autobot.toml`:
- Add one `[[outdoor]]` per outdoor unit.
- Under each, add one `[[outdoor.unit]]` per head unit piped to it. Each unit needs its board's
  address and `api_encryption_key`.
- Fill in `[mqtt]` with your broker's details.

```bash
cargo run --release -- -c autobot.toml --check
```

```bash
cargo run --release -- -c autobot.toml
```

Or build the add-on image and run it with `-c /config/autobot.toml`.

**Either way, remove the esp105 boards from Home Assistant's ESPHome integration** (or never
add them). Otherwise HA and autobot both write the same climate entity and override each other.
The boards' other entities (vanes, sensors) aren't passed through yet; see [TODO.md](TODO.md).

## Tests

```bash
cargo test
```

## Status

- **Unit tests:** the arbiter, the service, config loading and message conversion.
- **Tried against a stand-in board:** a fake device built with ESPHome 2026.11's `host`
  platform, plus a local MQTT broker. That covered the Noise-encrypted connection, entity
  listing, state updates, commands from the Home Assistant side, and reconnecting after the
  device went away.
- **Not yet tried:** real esp105 boards, a real outdoor unit, the add-on image (no Docker on the
  development Mac), or a real Home Assistant.
- **Matter:** there is no frontend yet; the plan is in [docs/design.md](docs/design.md#matter).

## License

Copyright 2026 Jeff Hamilton. Apache License 2.0; see [LICENSE](LICENSE).
