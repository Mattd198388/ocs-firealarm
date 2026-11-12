# Fire Alarm for Open CAD Studio

AlarmCAD-style fire alarm design tools as an Open CAD Studio plugin.

- **Fire Alarm ribbon tab** with all NFPA 170 device symbols (26 blocks)
- **Click-to-place devices** with automatic SLC addressing (L1-001, L1-002, ...)
- Each device stores its type, address, circuit, candela, speaker tap and
  standby/alarm current (XDATA app `FIREALARM`, round-trips through DWG/DXF)
- **Device count** and **CSV export**
- **Battery calculation** (NFPA 72 standby/alarm durations, safety margin)
- **NAC voltage drop** per circuit (worst case, load lumped at end of line),
  using the length of the wire you drew or a length you type
- **Coverage**: smoke/heat detector circles (0.7 x listed spacing) and strobe
  room squares from the NFPA 72 single-appliance room tables, on layer FA-COVERAGE
- **Wire Circuit**: click devices in order to draw an SLC/NAC run on FA-WIRING;
  horns/strobes you wire take that NAC's name
- **Riser diagram**: panel plus one row per circuit with device symbols and counts

Current draws are typical values for a generic 24 VDC system. Verify them
against the datasheets for the devices you actually install.

## Install

In Open CAD Studio run `PLUGINS`, add this repository, and install the latest
release. Restart the app; a **Fire Alarm** tab appears.

## Commands

| Command | What it does |
|---|---|
| `FA_SETUP` | Loads the symbol blocks and FA layers into the drawing (runs automatically on first placement) |
| `FA_SETTINGS` | Symbol scale, SLC loop, next address, strobe candela, speaker tap, NAC circuit |
| `FA_<DEVICE>` | Place a device, e.g. `FA_SMOKE`, `FA_PULL`, `FA_HS-WALL` |
| `FA_COUNT` | Device count by type |
| `FA_EXPORT <path>` | Export the device list to CSV |
| `FA_BATTERY` | Battery calculation |
| `FA_VDROP` | NAC voltage drop |
| `FA_COVERAGE` | Draw (or redraw) detector and strobe coverage |
| `FA_WIRE` | Draw a circuit by clicking devices in order |
| `FA_RISER` | Draw a riser diagram at a picked point |

Coverage values: wall strobe 15 cd = 20x20 ft, 30 cd = 28x28, 75 cd = 45x45,
95 cd = 50x50, 110 cd = 54x54, 135 cd = 60x60, 185 cd = 70x70; ceiling strobe
(10 ft ceiling) 15 cd = 20x20, 30 cd = 30x30, 75 cd = 40x40, 95 cd+ = 50x50.
Check these against the NFPA 72 edition your AHJ enforces.
