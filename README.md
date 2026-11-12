# Fire Alarm for Open CAD Studio

AlarmCAD-style fire alarm design tools as an Open CAD Studio plugin.

- **Fire Alarm ribbon tab** with all NFPA 170 device symbols (26 blocks)
- **Click-to-place devices** with automatic SLC addressing (L1-001, L1-002, ...)
- Each device stores its type, address, circuit, candela, speaker tap and
  standby/alarm current (XDATA app `FIREALARM`, round-trips through DWG/DXF)
- **Device count** and **CSV export**
- **Battery calculation** (NFPA 72 standby/alarm durations, safety margin)
- **NAC voltage drop** per circuit (worst case, load lumped at end of line)

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
