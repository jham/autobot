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
- Once it has run on real boards, tag `v0.1.0` so CI publishes the add-on images to
  `ghcr.io/jham/autobot-{amd64,aarch64}` and creates a GitHub Release. Then check that both GHCR
  packages are public (package settings on github.com), or the Supervisor can't pull them. CI
  already builds both images on every push, so the Dockerfile is known to work.
- Check on real boards that cn105's vane select options and the outside temperature come
  through, and that a climate command from autobot leaves the vane where it was.
- Install the add-on on the real Home Assistant and check the entities, the settings form and
  the Supervisor MQTT lookup.

## Features

- Mirror the boards' remaining entities (compressor frequency, input power, energy, runtime
  hours, stage, i-see) so the boards can stay out of HA's ESPHome integration with nothing
  lost. Vanes and outside temperature are done.
- Pass a remote room temperature sensor through to the board (cn105 supports one).
- Save the arbiter's side and turn timer across restarts.
- Optional per-unit priority weight (e.g. a nursery wins ties).
- Matter bridge frontend on rs-matter (see docs/design.md#matter).
- Resolve `.local` names with mDNS inside autobot (the `esphome-client` crate's `discovery`
  feature), so boards work by name even where the container has no mDNS resolver.
- Move to a newer ESPHome API version when `esphome-client` adds one.
- Backend for boards running the esp105 Matter firmware.
