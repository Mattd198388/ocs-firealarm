//! Prescriptive coverage rules used by FA_COVERAGE. Pure data, no host
//! dependency. Values follow the NFPA 72 room-spacing tables for a single
//! visible appliance; always check them against the edition your AHJ enforces.

/// Wall-mounted strobe, one per room at the middle of a wall:
/// (max square room side in ft, minimum candela).
const WALL: &[(f64, u32)] = &[
    (20.0, 15),
    (28.0, 30),
    (30.0, 34),
    (40.0, 60),
    (45.0, 75),
    (50.0, 94),
    (54.0, 110),
    (55.0, 115),
    (60.0, 135),
    (63.0, 150),
    (68.0, 177),
    (70.0, 184),
];

/// Ceiling-mounted strobe at the center of the room, ceiling height up to 10 ft:
/// (max square room side in ft, minimum candela).
const CEILING_10FT: &[(f64, u32)] = &[(20.0, 15), (30.0, 30), (40.0, 60), (50.0, 95)];

fn largest_room(table: &[(f64, u32)], candela: u32) -> Option<f64> {
    table.iter().filter(|(_, cd)| *cd <= candela).map(|(side, _)| *side).fold(None, |a, s| Some(a.map_or(s, |a: f64| a.max(s))))
}

/// Largest square room (side, ft) one wall strobe of `candela` covers.
pub fn wall_strobe_room_ft(candela: u32) -> Option<f64> {
    largest_room(WALL, candela)
}

/// Largest square room (side, ft) one ceiling strobe of `candela` covers at a
/// ceiling of 10 ft or lower.
pub fn ceiling_strobe_room_ft(candela: u32) -> Option<f64> {
    largest_room(CEILING_10FT, candela)
}

/// Radius (ft) a spot detector covers on a smooth flat ceiling: every point
/// within 0.7 x the listed spacing.
pub fn detector_radius_ft(listed_spacing_ft: f64) -> f64 {
    0.7 * listed_spacing_ft
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_table() {
        assert_eq!(wall_strobe_room_ft(15), Some(20.0));
        assert_eq!(wall_strobe_room_ft(30), Some(28.0));
        assert_eq!(wall_strobe_room_ft(75), Some(45.0));
        assert_eq!(wall_strobe_room_ft(95), Some(50.0));
        assert_eq!(wall_strobe_room_ft(110), Some(54.0));
        assert_eq!(wall_strobe_room_ft(135), Some(60.0));
        assert_eq!(wall_strobe_room_ft(185), Some(70.0));
        assert_eq!(wall_strobe_room_ft(10), None);
    }

    #[test]
    fn ceiling_table() {
        assert_eq!(ceiling_strobe_room_ft(15), Some(20.0));
        assert_eq!(ceiling_strobe_room_ft(75), Some(40.0));
        assert_eq!(ceiling_strobe_room_ft(95), Some(50.0));
        assert_eq!(ceiling_strobe_room_ft(185), Some(50.0));
    }

    #[test]
    fn smoke_radius() {
        assert!((detector_radius_ft(30.0) - 21.0).abs() < 1e-9);
    }
}
