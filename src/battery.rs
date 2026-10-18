//! Battery charge for the header, on laptops, tablets and handhelds.
//!
//! Linux publishes batteries in `/sys/class/power_supply/*` (type "Battery").
//! Batteries of wireless mice, keyboards and game controllers show up there
//! too, marked `scope=Device`; they are skipped, so a desktop shows nothing.
//! Drivers report either energy (`energy_now`/`energy_full`, µWh, with
//! `power_now` in µW) or charge (`charge_*`, µAh, with `current_now` in µA);
//! both are handled. Several batteries (some ThinkPads) are added up.

use std::fs;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    Charging,
    Discharging,
    Full,
    /// On AC but not charging (charge limit reached, or the charger paused).
    NotCharging,
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Battery {
    pub percent: f32,
    pub state: State,
    /// Charge or discharge rate, when the driver reports it.
    pub watts: Option<f32>,
    /// Hours until empty (discharging) or full (charging).
    pub hours_left: Option<f32>,
}

fn read(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn num(dir: &Path, f: &str) -> Option<f64> {
    read(&dir.join(f))?.parse::<f64>().ok()
}

/// The system battery (all of them combined), or None when there is none.
/// `root` is normally `/`; tests point it at a fake tree.
pub fn read_battery(root: &Path) -> Option<Battery> {
    let mut dirs: Vec<_> = fs::read_dir(root.join("sys/class/power_supply"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|d| {
            read(&d.join("type")).as_deref() == Some("Battery")
                && read(&d.join("scope")).as_deref() != Some("Device")
                && read(&d.join("present")).as_deref() != Some("0")
        })
        .collect();
    if dirs.is_empty() {
        return None;
    }
    dirs.sort();

    // Totals in µWh / µW when every battery reports energy, else in µAh / µA.
    let (mut now, mut full, mut rate, mut pcts) = (0.0, 0.0, 0.0, Vec::new());
    let mut states = Vec::new();
    for d in &dirs {
        let (n, f) = match (num(d, "energy_now"), num(d, "energy_full")) {
            (Some(n), Some(f)) => (Some(n), Some(f)),
            _ => (num(d, "charge_now"), num(d, "charge_full")),
        };
        if let (Some(n), Some(f)) = (n, f) {
            now += n;
            full += f;
        }
        // Rate in the same unit as now/full: power for energy, current for charge.
        let r = if d.join("energy_now").exists() { num(d, "power_now") } else { num(d, "current_now") };
        rate += r.unwrap_or(0.0).abs();
        if let Some(c) = num(d, "capacity") {
            pcts.push(c);
        }
        states.push(match read(&d.join("status")).as_deref() {
            Some("Charging") => State::Charging,
            Some("Discharging") => State::Discharging,
            Some("Full") => State::Full,
            Some("Not charging") => State::NotCharging,
            _ => State::Unknown,
        });
    }
    let percent = if full > 0.0 {
        (now / full * 100.0).clamp(0.0, 100.0)
    } else if !pcts.is_empty() {
        pcts.iter().sum::<f64>() / pcts.len() as f64
    } else {
        return None;
    };
    let state = [State::Discharging, State::Charging, State::NotCharging, State::Full]
        .into_iter()
        .find(|s| states.contains(s))
        .unwrap_or(State::Unknown);

    // Watts: power_now directly, or current × voltage for charge-based drivers.
    let watts = dirs
        .iter()
        .map(|d| {
            num(d, "power_now").or_else(|| Some(num(d, "current_now")? * num(d, "voltage_now")? / 1e6)).map(|w| w.abs() / 1e6)
        })
        .sum::<Option<f64>>()
        .filter(|w| *w > 0.05);
    let hours_left = (rate > 0.0 && full > 0.0)
        .then(|| match state {
            State::Discharging => Some(now / rate),
            State::Charging => Some((full - now).max(0.0) / rate),
            _ => None,
        })
        .flatten()
        .filter(|h| *h < 100.0);

    Some(Battery { percent: percent as f32, state, watts: watts.map(|w| w as f32), hours_left: hours_left.map(|h| h as f32) })
}

/// "2h 05m", "45m".
pub fn fmt_hours(h: f32) -> String {
    let m = (h * 60.0).round() as u32;
    if m >= 60 { format!("{}h {:02}m", m / 60, m % 60) } else { format!("{m}m") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supply(root: &Path, name: &str, files: &[(&str, &str)]) {
        let d = root.join("sys/class/power_supply").join(name);
        fs::create_dir_all(&d).unwrap();
        for (f, v) in files {
            fs::write(d.join(f), format!("{v}\n")).unwrap();
        }
    }

    #[test]
    fn laptop_battery_but_not_a_mouse_or_charger() {
        let tmp = std::env::temp_dir().join(format!("oryn-bat-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        // A desktop: charger-less, a wireless mouse battery. Nothing to show.
        supply(&tmp, "hidpp_battery_0", &[("type", "Battery"), ("scope", "Device"), ("capacity", "40"), ("status", "Discharging")]);
        assert_eq!(read_battery(&tmp), None);
        assert_eq!(read_battery(&tmp.join("nothing")), None);

        // Laptop on battery, energy-based driver: 30 of 60 Wh at 10 W = 3 h.
        supply(&tmp, "AC", &[("type", "Mains"), ("online", "0")]);
        supply(&tmp, "BAT0", &[
            ("type", "Battery"), ("present", "1"), ("status", "Discharging"), ("capacity", "50"),
            ("energy_now", "30000000"), ("energy_full", "60000000"), ("power_now", "10000000"),
        ]);
        let b = read_battery(&tmp).unwrap();
        assert_eq!((b.percent, b.state, b.watts, b.hours_left), (50.0, State::Discharging, Some(10.0), Some(3.0)));

        // Charging, charge-based driver: 2000 of 4000 mAh at 1000 mA, 15 V = 15 W, 2 h to full.
        let _ = fs::remove_dir_all(tmp.join("sys/class/power_supply/BAT0"));
        supply(&tmp, "BAT1", &[
            ("type", "Battery"), ("status", "Charging"), ("charge_now", "2000000"), ("charge_full", "4000000"),
            ("current_now", "1000000"), ("voltage_now", "15000000"),
        ]);
        let b = read_battery(&tmp).unwrap();
        assert_eq!((b.percent, b.state, b.watts, b.hours_left), (50.0, State::Charging, Some(15.0), Some(2.0)));

        // Full on AC: no rate, no time.
        supply(&tmp, "BAT1", &[("status", "Full"), ("charge_now", "4000000"), ("current_now", "0")]);
        let b = read_battery(&tmp).unwrap();
        assert_eq!((b.percent, b.state, b.watts, b.hours_left), (100.0, State::Full, None, None));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn time_formatting() {
        assert_eq!(fmt_hours(2.0833), "2h 05m");
        assert_eq!(fmt_hours(0.75), "45m");
    }
}
