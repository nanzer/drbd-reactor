use crate::drbd::{EventType, PluginUpdate};
use crate::plugin;
use anyhow::Result;
use log::{info, trace};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;

pub fn run(cfg: PromoterOpt, rx: Receiver<PluginUpdate>) -> Result<()> {
    trace!("promoter: start");

    let type_exists = plugin::typefilter(&EventType::Exists);
    let type_change = plugin::typefilter(&EventType::Change);
    let names = cfg.resources.keys().cloned().collect::<Vec<String>>();
    let names = plugin::namefilter(&names);

    // set default stop actions (i.e., reversed start, and default on-stop-failure (i.e., true)
    let cfg = {
        let mut cfg = cfg.clone();
        for res in cfg.resources.values_mut() {
            if res.stop.is_empty() {
                res.stop = res.start.clone();
                res.stop.reverse();
            }
            if res.on_stop_failure == "" {
                res.on_stop_failure = "true".to_string();
            }
        }
        cfg
    };

    for r in rx
        .into_iter()
        .filter(names)
        .filter(|x| type_exists(x) || type_change(x))
    {
        let name = r.get_name();
        let res = cfg
            .resources
            .get(&name)
            .expect("Can not happen, name filter is built from the cfg");

        match r {
            PluginUpdate::Resource {
                ref old, ref new, ..
            } => {
                if !old.may_promote && new.may_promote {
                    info!("promoter: resource '{}' may promote", name);
                    if start_actions(&res.start).is_err() && stop_actions(&res.stop).is_err() {
                        on_failure(&res.on_stop_failure); // loops until success
                    }
                }
            }
            PluginUpdate::Device {
                ref old, ref new, ..
            } => {
                if old.quorum && !new.quorum {
                    info!("promoter: resource '{}' lost quorum", name);
                    if stop_actions(&res.stop).is_err() {
                        on_failure(&res.on_stop_failure); // loops until success
                    }
                }
            }
            _ => (),
        }
    }

    trace!("promoter: exit");
    Ok(())
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PromoterOpt {
    #[serde(default)]
    pub resources: HashMap<String, PromoterOptResource>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct PromoterOptResource {
    #[serde(default)]
    pub start: Vec<String>,
    #[serde(default)]
    pub stop: Vec<String>,
    #[serde(default)]
    pub on_stop_failure: String,
}

fn systemd_stop(unit: &str) -> Result<()> {
    info!("promoter: systemctl stop {}", unit);
    plugin::map_status(Command::new("systemctl").arg("stop").arg(unit).status())
}

fn systemd_start(unit: &str) -> Result<()> {
    // we really don't care
    let _ = Command::new("systemctl")
        .arg("reset-failed")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .arg(unit)
        .status();

    info!("promoter: systemctl start {}", unit);
    plugin::map_status(Command::new("systemctl").arg("start").arg(unit).status())
}

fn action(to: State, what: &str) -> Result<()> {
    let words = what.split_whitespace().collect::<Vec<&str>>();
    if words.is_empty() {
        return Err(anyhow::anyhow!("action is empty"));
    }

    if Path::new(words[0]).is_absolute() {
        return plugin::system(what);
    }

    match to {
        State::Start => systemd_start(what),
        State::Stop => systemd_stop(what),
    }
}

fn start_actions(actions: &[String]) -> Result<()> {
    for a in actions {
        action(State::Start, a)?;
    }
    Ok(())
}

fn stop_actions(actions: &[String]) -> Result<()> {
    for a in actions {
        action(State::Stop, a)?;
    }
    Ok(())
}

pub fn on_failure(action: &str) {
    info!("promoter: starting on-failure action in a loop");
    loop {
        if plugin::system(action).is_ok() {
            return;
        }
        thread::sleep(Duration::from_secs(2));
    }
}

enum State {
    Start,
    Stop,
}
