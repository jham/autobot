# autobot

autobot keeps every head unit on a Mitsubishi multi-split outdoor unit heating together or
cooling together. Each head unit needs an [esp105](https://github.com/jham/esp105) board
running the Wi-Fi (ESPHome) firmware.

Home Assistant gets one climate entity per head unit. Its **heat/cool** mode is autobot's
AUTO, with a low and a high setpoint. autobot turns it into plain HEAT or COOL in step with the
rest of the outdoor unit. When rooms want opposite things, it gives them turns.

## Before you start

- Install the **Mosquitto broker** add-on and the **MQTT** integration. autobot gets the
  broker's details from the Supervisor, so there's nothing to enter.
- **Remove the esp105 boards from the ESPHome integration**, or don't add them. If both
  Home Assistant and autobot control a board, they keep overriding each other.

## Configuration

Add one entry under **Outdoor units** for each outdoor unit, with its head units (one per
esp105 board) listed under `unit`:

```yaml
- id: upstairs
  name: Upstairs MXZ
  unit:
    - id: bedroom
      name: Bedroom
      host: 192.168.1.41
      encryption_key: <api_encryption_key from the board's secrets.yaml>
    - id: office
      name: Office
      host: 192.168.1.42
      encryption_key: <...>
```

`host` can also be the board's `.local` name. An IP address (a DHCP reservation) is more
dependable.

When autobot first starts, each unit takes its setting from whatever the head unit is doing,
so starting autobot changes nothing.

## How it decides

| Setting | Default | Meaning |
|---|---|---|
| `start_offset` | 0.5 °C | How far past its setpoint a room must be before it calls for heat or cool |
| `min_run_min` | 15 | Shortest time on one side before switching while that side still calls |
| `max_slice_min` | 40 | Longest time on one side while the other side waits |
| `idle_switch_delay_min` | 3 | How long the current side must be satisfied before handing over |
| `demand_ratio` | 2.0 | After `min_run_min`, switch early if the other side calls this many times harder |
| `changeover_settle` | 60 s | Every unit is parked this long between heat and cool |
| `idle_action` | off | What a unit set to plain HEAT or COOL does while waiting: `off` or `fan` |

Put these under **Timing defaults** to change them everywhere. Put them in an outdoor unit's
entry to change them just for that unit.

Each outdoor unit also gets a **Mode** sensor (`heat`, `cool`, `changing to …`) and, once a
head unit reports one, an **Outside temperature** sensor. Each head unit gets a **Waiting for
outdoor unit** sensor and a **Vertical vane** select (and **Horizontal vane**, disabled by
default, for units that have one). Vane changes go straight to the head unit.
