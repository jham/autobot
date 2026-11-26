//! autobot: coordinates AUTO mode across the head units on a Mitsubishi multi-split outdoor unit.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc;
use tracing::info;
use tracing_subscriber::EnvFilter;

use autobot::backends::esphome::EspHomeUnit;
use autobot::config::Config;
use autobot::frontends::mqtt_ha::MqttHomeAssistant;
use autobot::service::{Backend, Frontend, Service};
use autobot::supervisor;

const USAGE: &str = "\
usage: autobot [-c CONFIG] [--check] [-v]
       autobot --addon

  -c, --config CONFIG  config file, .toml or .json (default: autobot.toml)
      --check          load the config, print a summary and exit
      --addon          run as a Home Assistant add-on: read /data/options.json and get the
                       MQTT broker from the Supervisor unless the options name one
  -v, --verbose        debug logging (or set RUST_LOG)";

struct Args {
    config: PathBuf,
    check: bool,
    addon: bool,
    verbose: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        config: "autobot.toml".into(),
        check: false,
        addon: false,
        verbose: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-c" | "--config" => a.config = it.next().context("--config needs a path")?.into(),
            "--check" => a.check = true,
            "--addon" => (a.addon, a.config) = (true, "/data/options.json".into()),
            "-v" | "--verbose" => a.verbose = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown argument {other:?}\n\n{USAGE}"),
        }
    }
    Ok(a)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let mut cfg = Config::load(&args.config)?;

    let level = if args.verbose {
        "debug"
    } else {
        cfg.log_level.as_deref().unwrap_or("info")
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("warn,autobot={level}")));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    if args.check {
        for o in &cfg.outdoor {
            println!("{} ({}): {}", o.id, o.name, o.units.join(", "));
            println!("  {:?}", o.arbiter);
        }
        match &cfg.mqtt {
            Some(m) => println!("mqtt: {}:{}", m.host, m.port),
            None if args.addon => println!("mqtt: from the Supervisor"),
            None => println!("mqtt: off"),
        }
        return Ok(());
    }

    if cfg.mqtt.is_none() && args.addon {
        let token = std::env::var("SUPERVISOR_TOKEN").context("SUPERVISOR_TOKEN is not set")?;
        cfg.mqtt = Some(supervisor::mqtt_service(&token).await?);
    }

    let (events_tx, events) = mpsc::unbounded_channel();
    let mut frontends: Vec<Box<dyn Frontend>> = Vec::new();
    if let Some(m) = cfg.mqtt.clone() {
        frontends.push(Box::new(MqttHomeAssistant::spawn(
            &cfg,
            m,
            events_tx.clone(),
        )));
    } else {
        info!("no [mqtt] section: running without a frontend");
    }
    let backends: BTreeMap<String, Box<dyn Backend>> = cfg
        .units
        .values()
        .map(|u| {
            (
                u.id.clone(),
                Box::new(EspHomeUnit::spawn(u.clone(), events_tx.clone())) as Box<dyn Backend>,
            )
        })
        .collect();

    info!(
        "autobot {}: {} outdoor unit(s), {} head unit(s)",
        env!("CARGO_PKG_VERSION"),
        cfg.outdoor.len(),
        cfg.units.len()
    );
    let service = Service::new(cfg, backends, frontends);
    tokio::select! {
        () = service.run(events) => {}
        r = shutdown() => { r?; info!("stopping"); }
    }
    Ok(())
}

/// Ctrl-C, or SIGTERM from Docker and the Home Assistant Supervisor.
async fn shutdown() -> Result<()> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            r = tokio::signal::ctrl_c() => r?,
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}
