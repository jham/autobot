# To do

Open work as of 2026-11-26.

## Before first use

- Try it against real esp105 boards (Wi-Fi firmware) once the 0.3.0 boards are installed:
  - Check that cn105 reports `action` (HEATING, COOLING, IDLE) reliably. The arbiter's startup
    side detection and the waiting display depend on it.
  - Check how the head unit behaves when switched between HEAT, COOL and OFF often. Tune
    `min_run`, `max_slice` and `changeover_settle` to suit.
  - Check how the outdoor unit reacts to a changeover with every head unit parked OFF versus
    FAN. Pick the default `idle_action` from that.
- Create the GitHub repo (`jham/autobot` is assumed in `repository.yaml`, `addon/config.yaml`
  and `addon/Dockerfile`), push, and tag `v0.1.0` so CI publishes the add-on images. The
  Dockerfile hasn't been built yet (no Docker on the development Mac); the first CI run is its
  first build.
- Install the add-on on the real Home Assistant and check the entities, the settings form and
  the Supervisor MQTT lookup.

## Features

- Mirror the boards' other entities (vertical and horizontal vane, outside air temperature,
  compressor frequency, energy) so the boards can stay out of HA's ESPHome integration with
  nothing lost.
- Pass a remote room temperature sensor through to the board (cn105 supports one).
- Save the arbiter's side and turn timer across restarts.
- Optional per-unit priority weight (e.g. a nursery wins ties).
- Matter bridge frontend on rs-matter (see docs/design.md#matter).
- Resolve `.local` names with mDNS inside autobot (the `esphome-client` crate's `discovery`
  feature), so boards work by name even where the container has no mDNS resolver.
- Move to a newer ESPHome API version when `esphome-client` adds one.
- Backend for boards running the esp105 Matter firmware.
