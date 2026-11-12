//! Battery and voltage-drop calculations. Pure math, no host dependency.

/// Copper conductor resistance, ohms per 1000 ft, solid uncoated
/// (NEC Chapter 9, Table 8, at 75 C).
pub fn ohms_per_kft(awg: u32) -> Option<f64> {
    Some(match awg {
        18 => 7.77,
        16 => 4.89,
        14 => 3.07,
        12 => 1.93,
        10 => 1.21,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct BatteryInput {
    /// Total standby current, mA (devices + panel).
    pub standby_ma: f64,
    /// Total alarm current, mA (devices + panel).
    pub alarm_ma: f64,
    /// Standby duration, hours (NFPA 72: 24 h typical).
    pub standby_hours: f64,
    /// Alarm duration, minutes (NFPA 72: 5 min, 15 min for voice).
    pub alarm_minutes: f64,
    /// Safety margin, fraction (0.20 = 20%).
    pub margin: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct BatteryResult {
    pub standby_ah: f64,
    pub alarm_ah: f64,
    pub total_ah: f64,
    pub required_ah: f64,
}

pub fn battery(input: BatteryInput) -> BatteryResult {
    let standby_ah = input.standby_ma / 1000.0 * input.standby_hours;
    let alarm_ah = input.alarm_ma / 1000.0 * input.alarm_minutes / 60.0;
    let total_ah = standby_ah + alarm_ah;
    BatteryResult { standby_ah, alarm_ah, total_ah, required_ah: total_ah * (1.0 + input.margin) }
}

/// Next standard battery size (Ah) at or above `required`.
pub fn standard_battery(required_ah: f64) -> Option<f64> {
    [7.0, 8.0, 12.0, 18.0, 26.0, 33.0, 40.0, 55.0, 75.0, 100.0]
        .into_iter()
        .find(|s| *s + 1e-9 >= required_ah)
}

#[derive(Debug, Clone, Copy)]
pub struct VdropInput {
    /// Circuit alarm current, A.
    pub amps: f64,
    /// One-way length of the circuit, ft.
    pub length_ft: f64,
    pub awg: u32,
    /// Panel output voltage at end of battery life (20.4 V typical for 24 V).
    pub source_volts: f64,
    /// Lowest allowed device voltage (16 V typical for 24 V devices).
    pub min_volts: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct VdropResult {
    pub drop_volts: f64,
    pub end_volts: f64,
    pub drop_percent: f64,
    pub ok: bool,
    /// Longest one-way run that stays at or above `min_volts`, ft.
    pub max_length_ft: f64,
}

/// Worst-case (all load lumped at the end of the circuit) voltage drop.
pub fn voltage_drop(input: VdropInput) -> Option<VdropResult> {
    let r_kft = ohms_per_kft(input.awg)?;
    let loop_ohms = 2.0 * input.length_ft * r_kft / 1000.0;
    let drop_volts = input.amps * loop_ohms;
    let end_volts = input.source_volts - drop_volts;
    let allowed = input.source_volts - input.min_volts;
    let max_length_ft = if input.amps > 0.0 {
        allowed / input.amps / (2.0 * r_kft / 1000.0)
    } else {
        f64::INFINITY
    };
    Some(VdropResult {
        drop_volts,
        end_volts,
        drop_percent: drop_volts / input.source_volts * 100.0,
        ok: end_volts >= input.min_volts,
        max_length_ft,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_basic() {
        let r = battery(BatteryInput {
            standby_ma: 200.0,
            alarm_ma: 2000.0,
            standby_hours: 24.0,
            alarm_minutes: 5.0,
            margin: 0.2,
        });
        assert!((r.standby_ah - 4.8).abs() < 1e-9);
        assert!((r.alarm_ah - 2.0 * 5.0 / 60.0).abs() < 1e-9);
        assert!((r.required_ah - (4.8 + 2.0 / 12.0) * 1.2).abs() < 1e-9);
        assert_eq!(standard_battery(r.required_ah), Some(7.0));
    }

    #[test]
    fn vdrop_basic() {
        // 1.5 A, 300 ft of 14 AWG: 2*300*3.07/1000 = 1.842 ohm -> 2.763 V
        let r = voltage_drop(VdropInput {
            amps: 1.5,
            length_ft: 300.0,
            awg: 14,
            source_volts: 20.4,
            min_volts: 16.0,
        })
        .unwrap();
        assert!((r.drop_volts - 2.763).abs() < 1e-3);
        assert!(r.ok);
        assert!(r.max_length_ft > 300.0);
    }
}
