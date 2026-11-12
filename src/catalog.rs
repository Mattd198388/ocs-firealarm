//! Device catalog: every NFPA 170 symbol the plugin can place, with the
//! electrical defaults used by the calcs. Pure data, no host dependency.
//!
//! Current draws are TYPICAL values for a generic 24 VDC addressable system.
//! Every placed device stores its own values, so they can be changed per
//! device to match the real datasheet.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Initiating,
    Notification,
    Module,
    Panel,
    Misc,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Initiating => "Initiating",
            Category::Notification => "Notification",
            Category::Module => "Modules",
            Category::Panel => "Panels",
            Category::Misc => "Other",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceType {
    /// Stable key stored in the drawing (XDATA class key) and command suffix.
    pub key: &'static str,
    /// Block name in the symbol library.
    pub block: &'static str,
    pub label: &'static str,
    pub short: &'static str,
    pub category: Category,
    /// Gets an SLC address when placed.
    pub addressable: bool,
    /// Has a strobe (candela setting).
    pub strobe: bool,
    /// Has a speaker (wattage tap).
    pub speaker: bool,
    /// Standby current, mA (SLC or panel bus).
    pub standby_ma: f64,
    /// Alarm current, mA, for non-strobe devices. Strobes use the candela table.
    pub alarm_ma: f64,
}

const fn dev(
    key: &'static str,
    block: &'static str,
    label: &'static str,
    short: &'static str,
    category: Category,
    addressable: bool,
    strobe: bool,
    speaker: bool,
    standby_ma: f64,
    alarm_ma: f64,
) -> DeviceType {
    DeviceType { key, block, label, short, category, addressable, strobe, speaker, standby_ma, alarm_ma }
}

use Category::*;

pub static DEVICES: &[DeviceType] = &[
    // Initiating (addressable SLC devices)
    dev("SMOKE", "FA-SMOKE", "Smoke Detector", "Smoke", Initiating, true, false, false, 0.3, 0.3),
    dev("HEAT", "FA-HEAT", "Heat Detector", "Heat", Initiating, true, false, false, 0.3, 0.3),
    dev("CO", "FA-CO", "Carbon Monoxide Detector", "CO", Initiating, true, false, false, 0.3, 0.3),
    dev("FLAME", "FA-FLAME", "Flame Detector", "Flame", Initiating, true, false, false, 0.3, 0.3),
    dev("DUCT", "FA-DUCT-SMOKE", "Duct Smoke Detector", "Duct", Initiating, true, false, false, 0.5, 0.5),
    dev("PULL", "FA-PULL", "Manual Pull Station", "Pull", Initiating, true, false, false, 0.3, 0.3),
    dev("RTS", "FA-RTS", "Remote Test Station", "RTS", Initiating, false, false, false, 0.0, 0.0),
    // Modules
    dev("AIM", "FA-AIM", "Addressable Input Module", "Input Mod", Module, true, false, false, 0.4, 0.4),
    dev("AOM", "FA-AOM", "Addressable Output Module", "Output Mod", Module, true, false, false, 0.4, 0.4),
    dev("EOL", "FA-EOL", "End-of-Line Device", "EOL", Module, false, false, false, 0.0, 0.0),
    // Notification (NAC devices; strobe current comes from candela table)
    dev("HS-WALL", "FA-HORN-STROBE-WALL", "Wall Horn/Strobe", "H/S Wall", Notification, false, true, false, 0.0, 0.0),
    dev("HS-CLG", "FA-HORN-STROBE-CLG", "Ceiling Horn/Strobe", "H/S Clg", Notification, false, true, false, 0.0, 0.0),
    dev("S-WALL", "FA-STROBE-WALL", "Wall Strobe", "Strobe Wall", Notification, false, true, false, 0.0, 0.0),
    dev("S-CLG", "FA-STROBE-CLG", "Ceiling Strobe", "Strobe Clg", Notification, false, true, false, 0.0, 0.0),
    dev("H-WALL", "FA-HORN-WALL", "Wall Horn", "Horn Wall", Notification, false, false, false, 0.0, 22.0),
    dev("H-CLG", "FA-HORN-CLG", "Ceiling Horn", "Horn Clg", Notification, false, false, false, 0.0, 22.0),
    dev("SS-WALL", "FA-SPK-STROBE-WALL", "Wall Speaker/Strobe", "Spk/S Wall", Notification, false, true, true, 0.0, 0.0),
    dev("SS-CLG", "FA-SPK-STROBE-CLG", "Ceiling Speaker/Strobe", "Spk/S Clg", Notification, false, true, true, 0.0, 0.0),
    dev("SPK-WALL", "FA-SPK-WALL", "Wall Speaker", "Spk Wall", Notification, false, false, true, 0.0, 0.0),
    dev("SPK-CLG", "FA-SPK-CLG", "Ceiling Speaker", "Spk Clg", Notification, false, false, true, 0.0, 0.0),
    dev("BELL", "FA-BELL", "Bell", "Bell", Notification, false, false, false, 0.0, 30.0),
    dev("RI-WALL", "FA-RI-WALL", "Wall Remote Indicator", "RI Wall", Misc, false, false, false, 0.0, 10.0),
    dev("RI-CLG", "FA-RI-CLG", "Ceiling Remote Indicator", "RI Clg", Misc, false, false, false, 0.0, 10.0),
    // Panels / misc
    dev("FACP", "FA-FACP", "Fire Alarm Control Panel", "FACP", Panel, false, false, false, 0.0, 0.0),
    dev("ANN", "FA-TEXT-SIGN", "Text Sign / Annunciator", "Sign", Panel, false, false, false, 0.0, 0.0),
    dev("PHONE", "FA-FIREPHONE", "Firefighter Phone", "Phone", Misc, false, false, false, 0.0, 0.0),
];

pub fn by_key(key: &str) -> Option<&'static DeviceType> {
    DEVICES.iter().find(|d| d.key.eq_ignore_ascii_case(key))
}

/// Strobe candela settings offered by most multi-candela strobes.
pub const CANDELAS: &[u32] = &[15, 30, 75, 95, 110, 135, 185];

/// Typical 24 VDC (regulated) alarm current in mA for a horn/strobe or strobe
/// at each candela. Generic values in line with common multi-candela
/// products; verify against the actual device datasheet.
pub fn strobe_ma(candela: u32) -> f64 {
    match candela {
        0..=15 => 66.0,
        16..=30 => 94.0,
        31..=75 => 158.0,
        76..=95 => 181.0,
        96..=110 => 210.0,
        111..=135 => 245.0,
        _ => 317.0,
    }
}

/// Alarm current for a device with the given settings, mA.
pub fn alarm_current_ma(dev: &DeviceType, candela: u32) -> f64 {
    if dev.strobe {
        strobe_ma(candela) + if dev.key.starts_with("HS") { 6.0 } else { 0.0 }
    } else {
        dev.alarm_ma
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_unique() {
        for (i, a) in DEVICES.iter().enumerate() {
            for b in &DEVICES[i + 1..] {
                assert_ne!(a.key, b.key);
                assert_ne!(a.block, b.block);
            }
        }
    }

    #[test]
    fn strobe_current_rises_with_candela() {
        let mut last = 0.0;
        for cd in CANDELAS {
            let ma = strobe_ma(*cd);
            assert!(ma > last);
            last = ma;
        }
    }
}
