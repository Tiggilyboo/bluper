use std::fs;
use std::path::Path;

/// Try to read host battery percentage as 0-100.
/// Returns None if not available on this platform or not found.
pub fn get_battery_percent() -> Option<u8> {
    #[cfg(target_os = "linux")]
    {
        return linux_sysfs_battery_percent();
    }

    #[cfg(all(not(target_os = "linux"), feature = "battery-crate"))]
    {
        return battery_crate_percent();
    }

    #[allow(unreachable_code)]
    None
}

#[cfg(target_os = "linux")]
fn linux_sysfs_battery_percent() -> Option<u8> {
    let root = Path::new("/sys/class/power_supply");
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        // Filter to type == Battery if possible
        let type_path = p.join("type");
        let ty = fs::read_to_string(&type_path).ok().map(|s| s.trim().to_owned());
        if let Some(t) = ty {
            if t != "Battery" { continue; }
        } else if !p.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with("BAT")).unwrap_or(false) {
            continue;
        }
        // capacity
        let cap_path = p.join("capacity");
        if let Ok(s) = fs::read_to_string(&cap_path) {
            let s = s.trim();
            if let Ok(v) = s.parse::<u8>() {
                return Some(v.min(100));
            }
        }
    }
    None
}

#[cfg(all(not(target_os = "linux"), feature = "battery-crate"))]
fn battery_crate_percent() -> Option<u8> {
    let manager = battery::Manager::new().ok()?;
    for b in manager.batteries().ok()?.flatten() {
        let v = b.state_of_charge().value() * 100.0;
        let pct = v.round() as i16; // allow bounds
        let pct = pct.clamp(0, 100) as u8;
        return Some(pct);
    }
    None
}
