//! Fire Alarm design tools for Open CAD Studio (AlarmCAD-style).
//!
//! Phase 1-3: NFPA 170 device placement with auto-addressing, device data on
//! every symbol (XDATA app `FIREALARM`), device counts / CSV export, battery
//! calcs and NAC voltage drop.

pub mod calc;
pub mod catalog;
pub mod coverage;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};

use catalog::{Category, DeviceType, DEVICES};
use ocs_plugin_api::dialog::{DialogControl, DialogDef, DialogResponse, DialogRow, DialogValue};
use ocs_plugin_api::export_plugin;
use ocs_plugin_api::host::acadrust::entities::{AttributeEntity, Circle, Insert, Line, LwPolyline, Text};
use ocs_plugin_api::host::acadrust::types::{Color, Vector2, Vector3};
use ocs_plugin_api::host::{
    BuiltinPlugin, CommandRequest, CommandStep, EntityType, ExtendedDataRecord, Handle, HostApi,
    InteractiveCommand, LayerConfig, PreviewWire, XDataValue,
};
use ocs_plugin_api::manifest::{ApiVersion, EntityClasses, PluginManifest};
use ocs_plugin_api::ribbon::{CadModule, IconKind, ModuleEvent, RibbonGroup, RibbonItem, ToolDef};

const PLUGIN_ID: &str = "matt.firealarm";
const APP: &str = "FIREALARM";
const CLASS_PREFIX: &str = "FA-DEVICE:";
const DEVICE_LAYER: &str = "FA-DEVICES";
const LIBRARY_FILE: &str = "fa_blocks.dxf";
const WIRING_LAYER: &str = "FA-WIRING";
const COVERAGE_LAYER: &str = "FA-COVERAGE";
const RISER_LAYER: &str = "FA-RISER";
const WIRE_PREFIX: &str = "FA-WIRE:";
const COVERAGE_TAG: &str = "FA-COVERAGE";

static MANIFEST: PluginManifest = PluginManifest {
    id: PLUGIN_ID,
    name: "Fire Alarm",
    version: env!("CARGO_PKG_VERSION"),
    description: "Fire alarm design: NFPA 170 devices, addressing, device counts, battery and voltage drop calcs.",
    api_version: ApiVersion::CURRENT,
    ribbon_order: 40,
    xdata_apps: &[APP],
    command_prefixes: &["FA_"],
};

// ───────────────────────────── settings ─────────────────────────────

#[derive(Debug, Clone)]
struct Settings {
    scale: f64,
    loop_no: u32,
    next_addr: Option<u32>,
    candela: u32,
    watts: String,
    nac: String,
    /// Drawing units per foot: 12 for inch drawings, 1 for foot drawings.
    units_per_ft: f64,
    smoke_spacing_ft: f64,
    heat_spacing_ft: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            scale: 1.0, loop_no: 1, next_addr: None, candela: 15, watts: "1/4".into(), nac: "NAC-1".into(),
            units_per_ft: 12.0, smoke_spacing_ft: 30.0, heat_spacing_ft: 50.0,
        }
    }
}

fn settings() -> &'static Mutex<Settings> {
    static S: OnceLock<Mutex<Settings>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Settings::default()))
}

// ───────────────────────────── device data ─────────────────────────────

#[derive(Debug, Clone, Default)]
struct DeviceData {
    key: String,
    addr: String,
    circuit: String,
    candela: u32,
    watts: String,
    standby_ma: f64,
    alarm_ma: f64,
}

impl DeviceData {
    fn to_record(&self) -> ExtendedDataRecord {
        let mut r = ExtendedDataRecord::new(APP);
        for v in [
            format!("{CLASS_PREFIX}{}", self.key),
            format!("ADDR={}", self.addr),
            format!("CKT={}", self.circuit),
            format!("CD={}", self.candela),
            format!("W={}", self.watts),
            format!("SBMA={}", self.standby_ma),
            format!("ALMA={}", self.alarm_ma),
        ] {
            r.add_value(XDataValue::String(v));
        }
        r
    }

    fn from_record(r: &ExtendedDataRecord) -> Option<Self> {
        let mut d = DeviceData::default();
        for v in &r.values {
            let XDataValue::String(s) = v else { continue };
            if let Some(k) = s.strip_prefix(CLASS_PREFIX) {
                d.key = k.to_string();
            } else if let Some((k, val)) = s.split_once('=') {
                match k {
                    "ADDR" => d.addr = val.to_string(),
                    "CKT" => d.circuit = val.to_string(),
                    "CD" => d.candela = val.parse().unwrap_or(0),
                    "W" => d.watts = val.to_string(),
                    "SBMA" => d.standby_ma = val.parse().unwrap_or(0.0),
                    "ALMA" => d.alarm_ma = val.parse().unwrap_or(0.0),
                    _ => {}
                }
            }
        }
        (!d.key.is_empty()).then_some(d)
    }

    fn device_type(&self) -> Option<&'static DeviceType> {
        catalog::by_key(&self.key)
    }
}

struct Placed {
    handle: Handle,
    data: DeviceData,
}

/// Every fire alarm device in the drawing, in drawing order.
fn collect_devices(host: &mut dyn HostApi) -> Vec<Placed> {
    let mut out = Vec::new();
    let mut offset = 0u64;
    loop {
        let q = format!(r#"{{"xdata_app":"{APP}","offset":{offset},"limit":2000}}"#);
        let page = match host.query_entities(&q) {
            Ok(p) => p,
            Err(e) => {
                host.push_error(&format!("Fire Alarm: device query failed: {e}"));
                break;
            }
        };
        for e in &page.entities {
            let handle = e.common().handle;
            if let Some(data) = host.read_record(handle, APP).and_then(DeviceData::from_record) {
                out.push(Placed { handle, data });
            }
        }
        match page.next_offset {
            Some(n) if n > offset => offset = n,
            _ => break,
        }
    }
    out
}

/// Parse "L1-007" → (1, 7).
fn parse_addr(a: &str) -> Option<(u32, u32)> {
    let rest = a.strip_prefix('L')?;
    let (l, n) = rest.split_once('-')?;
    Some((l.parse().ok()?, n.parse().ok()?))
}

fn next_address(host: &mut dyn HostApi, loop_no: u32) -> u32 {
    collect_devices(host)
        .iter()
        .filter_map(|p| parse_addr(&p.data.addr))
        .filter(|(l, _)| *l == loop_no)
        .map(|(_, n)| n)
        .max()
        .unwrap_or(0)
        + 1
}

// ───────────────────────────── setup / library ─────────────────────────────

fn library_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("OpenCADStudio").join("plugins").join(PLUGIN_ID).join(LIBRARY_FILE))
}

fn blocks_loaded(host: &dyn HostApi) -> bool {
    host.document().block_records.get("FA-SMOKE").is_some()
}

fn ensure_layers(host: &mut dyn HostApi) {
    for (name, color, desc) in [
        (DEVICE_LAYER, 1i16, "Fire alarm devices"),
        (WIRING_LAYER, 3, "Fire alarm wiring"),
        ("FA-TEXT", 7, "Fire alarm notes and tags"),
        (COVERAGE_LAYER, 4, "Detector and strobe coverage (turn off to hide)"),
        (RISER_LAYER, 7, "Fire alarm riser diagram"),
    ] {
        let exists = host.document().layers.get(name).is_some();
        if !exists {
            host.add_layer(LayerConfig {
                name: name.to_string(),
                color: Some(Color::from_index(color)),
                linetype: None,
                lineweight: None,
                off: None,
                frozen: None,
                locked: None,
                plottable: None,
                transparency: None,
                description: Some(desc.to_string()),
            });
        }
    }
}

/// Load the symbol blocks into the drawing (once). Returns true when ready.
fn ensure_setup(host: &mut dyn HostApi) -> bool {
    ensure_layers(host);
    if blocks_loaded(host) {
        return true;
    }
    // The library ships inside the plugin binary; write it out next to the
    // plugin (or refresh it when this build carries a different copy).
    static LIBRARY: &[u8] = include_bytes!("../assets/fa_blocks.dxf");
    let Some(path) = library_path() else {
        host.push_error("Fire Alarm: can't find the app's settings folder to unpack the symbol library.");
        return false;
    };
    let current = std::fs::read(&path).ok();
    if current.as_deref() != Some(LIBRARY) {
        let written = path.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::write(&path, LIBRARY));
        if let Err(e) = written {
            host.push_error(&format!("Fire Alarm: couldn't unpack the symbol library to {}: {e}", path.display()));
            return false;
        }
    }
    let p = path.display().to_string();
    if p.contains(' ') {
        host.push_error(&format!("Fire Alarm: can't auto-load symbols from a path with spaces ({p}). Insert that file once with INSERT, then try again."));
        return false;
    }
    host.push_undo("Load fire alarm symbols");
    let started = host.execute_command(&format!("_-INSERTFILE {p}"));
    // The import leaves INSERT waiting for a point; we only want the definitions.
    let _ = host.run_command(CommandRequest::Cancel);
    if started && blocks_loaded(host) {
        host.push_info("Fire Alarm: symbol library loaded into this drawing.");
        true
    } else {
        host.push_error(&format!(
            "Fire Alarm: couldn't load the symbol library automatically. Run INSERT, browse to {p}, place it anywhere, then erase it and try again."
        ));
        false
    }
}

// ───────────────────────────── placement ─────────────────────────────

struct PlaceCmd {
    dev: &'static DeviceType,
}

impl InteractiveCommand for PlaceCmd {
    fn prompt(&self) -> String {
        let s = settings().lock().unwrap().clone();
        let extra = if self.dev.addressable {
            format!(" (loop {}, next address auto)", s.loop_no)
        } else if self.dev.strobe {
            format!(" ({} cd on {})", s.candela, s.nac)
        } else if self.dev.category == Category::Notification {
            format!(" (on {})", s.nac)
        } else {
            String::new()
        };
        format!("Place {}{}. Click a location, Enter to finish", self.dev.label, extra)
    }

    fn on_point(&mut self, pt: [f64; 3]) -> CommandStep {
        CommandStep::Dispatch(format!("FA_PUT|{}|{}|{}", self.dev.key, pt[0], pt[1]))
    }

    fn on_enter(&mut self) -> CommandStep {
        CommandStep::Done
    }
}

fn build_insert(host: &dyn HostApi, dev: &DeviceType, at: Vector3, scale: f64, values: &[(&str, String)]) -> Insert {
    let mut ins = Insert::new(dev.block, at).with_uniform_scale(scale);
    ins.common.layer = DEVICE_LAYER.to_string();
    let doc = host.document();
    for e in doc.entities_in_block(dev.block) {
        if let EntityType::AttributeDefinition(def) = e {
            if def.is_constant() {
                continue;
            }
            let value = values.iter().find(|(t, _)| t.eq_ignore_ascii_case(&def.tag)).map(|(_, v)| v.clone());
            let mut att = AttributeEntity::from_definition(def, value);
            att.common.layer = DEVICE_LAYER.to_string();
            att.apply_insert_transform(at, Vector3::new(scale, scale, scale), 0.0);
            ins.attributes.push(att);
        }
    }
    ins
}

fn put_device(host: &mut dyn HostApi, key: &str, x: f64, y: f64) {
    let Some(dev) = catalog::by_key(key) else {
        host.push_error(&format!("Fire Alarm: unknown device {key}"));
        return;
    };
    let mut s = settings().lock().unwrap().clone();
    let mut data = DeviceData {
        key: dev.key.to_string(),
        standby_ma: dev.standby_ma,
        alarm_ma: catalog::alarm_current_ma(dev, s.candela),
        ..Default::default()
    };
    if dev.addressable {
        let n = match s.next_addr {
            Some(n) => n,
            None => next_address(host, s.loop_no),
        };
        data.addr = format!("L{}-{:03}", s.loop_no, n);
        data.circuit = format!("SLC-{}", s.loop_no);
        s.next_addr = Some(n + 1);
    } else if matches!(dev.category, Category::Notification) {
        data.circuit = s.nac.clone();
    }
    if dev.strobe {
        data.candela = s.candela;
    }
    if dev.speaker {
        data.watts = s.watts.clone();
    }
    *settings().lock().unwrap() = s.clone();

    let attrs = [
        ("ADDR", data.addr.clone()),
        ("CD", if dev.strobe { data.candela.to_string() } else { String::new() }),
        ("WW", data.watts.clone()),
    ];
    let ins = build_insert(host, dev, Vector3::new(x, y, 0.0), s.scale, &attrs);
    host.push_undo(&format!("Place {}", dev.label));
    let handle = host.add_entity(EntityType::Insert(ins));
    if !host.write_record(handle, data.to_record()) {
        host.push_error("Fire Alarm: placed the symbol but couldn't save its device data.");
    }
    host.set_dirty();
    let tag = if data.addr.is_empty() { data.circuit.clone() } else { data.addr.clone() };
    host.push_output(&format!("{} placed {}", dev.label, tag));
    host.start_interactive(Box::new(PlaceCmd { dev }));
}

fn start_place(host: &mut dyn HostApi, dev: &'static DeviceType) {
    if !ensure_setup(host) {
        return;
    }
    host.start_interactive(Box::new(PlaceCmd { dev }));
}


// ───────────────────────────── dialogs ─────────────────────────────
//
// The host delivers a submitted dialog only to an active interactive command,
// so each dialog runs inside a DialogCmd that forwards the values back to
// `dispatch` as "FA_DLG|<kind>|key=value;key=value".

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn value_text(v: &DialogValue) -> String {
    match v {
        DialogValue::Text(t) => t.clone(),
        DialogValue::Number(n) => n.to_string(),
        DialogValue::Bool(b) => b.to_string(),
        DialogValue::Choice { text, .. } => text.clone(),
        DialogValue::FilePath(p) => p.clone(),
        _ => String::new(),
    }
}

struct DialogCmd {
    kind: &'static str,
    def: Option<DialogDef>,
}

impl InteractiveCommand for DialogCmd {
    fn prompt(&self) -> String {
        "Fill in the dialog".into()
    }
    fn on_point(&mut self, _pt: [f64; 3]) -> CommandStep {
        CommandStep::NeedPoint
    }
    fn on_enter(&mut self) -> CommandStep {
        CommandStep::Done
    }
    fn modal_dialog(&self) -> Option<DialogDef> {
        self.def.clone()
    }
    fn on_dialog_response(&mut self, response: DialogResponse) -> CommandStep {
        self.def = None;
        match response {
            DialogResponse::Submitted(v) => {
                let body: Vec<String> = v.iter().map(|(k, val)| format!("{}={}", enc(k), enc(&value_text(val)))).collect();
                CommandStep::Dispatch(format!("FA_DLG|{}|{}", self.kind, body.join(";")))
            }
            DialogResponse::Cancelled => CommandStep::Cancel,
        }
    }
}

fn open_dialog(host: &mut dyn HostApi, kind: &'static str, def: DialogDef) {
    host.start_interactive(Box::new(DialogCmd { kind, def: Some(def) }));
}

/// Parse "k=v;k=v" from FA_DLG into a value map.
fn parse_dlg(body: &str) -> HashMap<String, DialogValue> {
    body.split(';')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (dec(k).to_ascii_lowercase(), DialogValue::Text(dec(v))))
        .collect()
}

// ───────────────────────────── settings dialog ─────────────────────────────

fn num(v: Option<&DialogValue>) -> Option<f64> {
    match v? {
        DialogValue::Number(n) => Some(*n),
        DialogValue::Text(t) => t.trim().parse().ok(),
        _ => None,
    }
}
fn text(v: Option<&DialogValue>) -> Option<String> {
    match v? {
        DialogValue::Text(t) => Some(t.trim().to_string()),
        DialogValue::Choice { text, .. } => Some(text.clone()),
        _ => None,
    }
}

fn settings_dialog(host: &mut dyn HostApi) {
    let s = settings().lock().unwrap().clone();
    let next = s.next_addr.unwrap_or_else(|| next_address(host, s.loop_no));
    let cd_idx = catalog::CANDELAS.iter().position(|c| *c == s.candela).unwrap_or(0);
    let units_idx = if s.units_per_ft == 1.0 { 1 } else { 0 };
    // Two rows only: the host's dialog window does not grow to fit more.
    let dialog = DialogDef::new("Fire Alarm Settings")
        .with_row(DialogRow::new(vec![
            DialogControl::Number { id: "scale".into(), label: "Symbol scale".into(), default: s.scale, min: Some(0.01), max: Some(1000.0), step: Some(0.25) },
            DialogControl::Choice { id: "units".into(), label: "Drawing units".into(), options: vec!["Inches".into(), "Feet".into()], default_index: units_idx },
            DialogControl::Number { id: "loop".into(), label: "SLC loop".into(), default: s.loop_no as f64, min: Some(1.0), max: Some(99.0), step: Some(1.0) },
            DialogControl::Number { id: "next".into(), label: "Next address".into(), default: next as f64, min: Some(1.0), max: Some(999.0), step: Some(1.0) },
            DialogControl::Choice {
                id: "cd".into(), label: "Candela".into(),
                options: catalog::CANDELAS.iter().map(|c| format!("{c}")).collect(), default_index: cd_idx,
            },
        ]))
        .with_row(DialogRow::new(vec![
            DialogControl::Text { id: "nac".into(), label: "NAC circuit".into(), default: s.nac.clone() },
            DialogControl::Text { id: "watts".into(), label: "Speaker tap (W)".into(), default: s.watts.clone() },
            DialogControl::Number { id: "smoke".into(), label: "Smoke spacing (ft)".into(), default: s.smoke_spacing_ft, min: Some(1.0), max: Some(100.0), step: Some(1.0) },
            DialogControl::Number { id: "heat".into(), label: "Heat spacing (ft)".into(), default: s.heat_spacing_ft, min: Some(1.0), max: Some(100.0), step: Some(1.0) },
        ]))
        .submit_label("Save")
        .width(900.0);
    open_dialog(host, "SETTINGS", dialog);
}

fn settings_apply(host: &mut dyn HostApi, v: &HashMap<String, DialogValue>) {
    let msg = {
        let mut s = settings().lock().unwrap();
        if let Some(x) = num(v.get("scale")).filter(|x| *x > 0.0) { s.scale = x; }
        if let Some(u) = text(v.get("units")) { s.units_per_ft = if u.eq_ignore_ascii_case("feet") { 1.0 } else { 12.0 }; }
        s.loop_no = num(v.get("loop")).map(|x| x.round().max(1.0) as u32).unwrap_or(s.loop_no);
        s.next_addr = num(v.get("next")).map(|x| x.round().max(1.0) as u32);
        if let Some(t) = text(v.get("cd")).and_then(|t| t.parse().ok()) { s.candela = t; }
        if let Some(t) = text(v.get("watts")).filter(|t| !t.is_empty()) { s.watts = t; }
        if let Some(t) = text(v.get("nac")).filter(|t| !t.is_empty()) { s.nac = t.to_uppercase(); }
        if let Some(x) = num(v.get("smoke")).filter(|x| *x > 0.0) { s.smoke_spacing_ft = x; }
        if let Some(x) = num(v.get("heat")).filter(|x| *x > 0.0) { s.heat_spacing_ft = x; }
        format!(
            "Fire Alarm settings: scale {}, {} drawing, loop {}, next address {}, {} cd, {} W, {}, smoke {} ft, heat {} ft",
            s.scale, if s.units_per_ft == 1.0 { "feet" } else { "inch" }, s.loop_no, s.next_addr.unwrap_or(1),
            s.candela, s.watts, s.nac, s.smoke_spacing_ft, s.heat_spacing_ft
        )
    };
    host.push_info(&msg);
}

// ───────────────────────────── reports ─────────────────────────────

fn device_count(host: &mut dyn HostApi) {
    let devs = collect_devices(host);
    if devs.is_empty() {
        host.push_info("Fire Alarm: no devices in this drawing yet.");
        return;
    }
    let mut counts: BTreeMap<(usize, String), usize> = BTreeMap::new();
    for p in &devs {
        let idx = DEVICES.iter().position(|d| d.key == p.data.key).unwrap_or(usize::MAX);
        let label = p.data.device_type().map(|d| d.label).unwrap_or("Unknown");
        let label = if p.data.candela > 0 { format!("{label} ({} cd)", p.data.candela) } else { label.to_string() };
        *counts.entry((idx, label)).or_default() += 1;
    }
    host.push_output("── Fire alarm device count ──");
    for ((_, label), n) in &counts {
        host.push_output(&format!("{n:>5}  {label}"));
    }
    host.push_output(&format!("{:>5}  TOTAL", devs.len()));
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

fn export_csv(host: &mut dyn HostApi, path: &str) {
    let devs = collect_devices(host);
    let mut out = String::from("Handle,Device,Category,Address,Circuit,Candela,Speaker W,Standby mA,Alarm mA,X,Y\n");
    let handles: Vec<Handle> = devs.iter().map(|p| p.handle).collect();
    let ents = host.entities_batch(&handles).unwrap_or_default();
    for (i, p) in devs.iter().enumerate() {
        let (x, y) = match ents.get(i).and_then(|e| e.as_ref()) {
            Some(EntityType::Insert(ins)) => (ins.insert_point.x, ins.insert_point.y),
            _ => (f64::NAN, f64::NAN),
        };
        let dt = p.data.device_type();
        let row = [
            format!("{}", p.handle),
            dt.map(|d| d.label).unwrap_or(&p.data.key).to_string(),
            dt.map(|d| d.category.label()).unwrap_or("").to_string(),
            p.data.addr.clone(),
            p.data.circuit.clone(),
            if p.data.candela > 0 { p.data.candela.to_string() } else { String::new() },
            p.data.watts.clone(),
            format!("{}", p.data.standby_ma),
            format!("{}", p.data.alarm_ma),
            format!("{x:.2}"),
            format!("{y:.2}"),
        ];
        out.push_str(&row.iter().map(|c| csv_escape(c)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    match std::fs::write(path, out) {
        Ok(_) => host.push_info(&format!("Fire Alarm: exported {} devices to {path}", devs.len())),
        Err(e) => host.push_error(&format!("Fire Alarm: couldn't write {path}: {e}")),
    }
}

// ───────────────────────────── calcs ─────────────────────────────

fn battery_calc(host: &mut dyn HostApi) {
    let devs = collect_devices(host);
    let dev_standby: f64 = devs.iter().map(|p| p.data.standby_ma).sum();
    let dev_alarm: f64 = devs.iter().map(|p| p.data.alarm_ma.max(p.data.standby_ma)).sum();
    let dialog = DialogDef::new("Battery Calculation")
        .with_row(DialogRow::single(DialogControl::InfoText {
            text: format!(
                "{} devices on this drawing: {:.1} mA standby, {:.1} mA alarm. Add the panel's own current from its datasheet.",
                devs.len(), dev_standby, dev_alarm
            ),
            align_h: None,
        }))
        .with_row(DialogRow::new(vec![
            DialogControl::Number { id: "psb".into(), label: "Panel standby (mA)".into(), default: 150.0, min: Some(0.0), max: None, step: Some(10.0) },
            DialogControl::Number { id: "pal".into(), label: "Panel alarm (mA)".into(), default: 300.0, min: Some(0.0), max: None, step: Some(10.0) },
        ]))
        .with_row(DialogRow::new(vec![
            DialogControl::Number { id: "hrs".into(), label: "Standby hours".into(), default: 24.0, min: Some(1.0), max: Some(168.0), step: Some(1.0) },
            DialogControl::Number { id: "min".into(), label: "Alarm minutes".into(), default: 5.0, min: Some(1.0), max: Some(120.0), step: Some(1.0) },
            DialogControl::Number { id: "mgn".into(), label: "Safety margin %".into(), default: 20.0, min: Some(0.0), max: Some(100.0), step: Some(5.0) },
        ]))
        .submit_label("Calculate");
    open_dialog(host, "BATTERY", dialog);
}

fn battery_apply(host: &mut dyn HostApi, v: &HashMap<String, DialogValue>) {
    let devs = collect_devices(host);
    let dev_standby: f64 = devs.iter().map(|p| p.data.standby_ma).sum();
    let dev_alarm: f64 = devs.iter().map(|p| p.data.alarm_ma.max(p.data.standby_ma)).sum();
    let input = calc::BatteryInput {
        standby_ma: dev_standby + num(v.get("psb")).unwrap_or(0.0),
        alarm_ma: dev_alarm + num(v.get("pal")).unwrap_or(0.0),
        standby_hours: num(v.get("hrs")).unwrap_or(24.0),
        alarm_minutes: num(v.get("min")).unwrap_or(5.0),
        margin: num(v.get("mgn")).unwrap_or(20.0) / 100.0,
    };
    let r = calc::battery(input);
    host.push_output("── Battery calculation ──");
    host.push_output(&format!("Standby: {:.1} mA x {} h = {:.2} Ah", input.standby_ma, input.standby_hours, r.standby_ah));
    host.push_output(&format!("Alarm:   {:.1} mA x {} min = {:.2} Ah", input.alarm_ma, input.alarm_minutes, r.alarm_ah));
    host.push_output(&format!("Total {:.2} Ah + {:.0}% margin = {:.2} Ah required", r.total_ah, input.margin * 100.0, r.required_ah));
    match calc::standard_battery(r.required_ah) {
        Some(s) => host.push_output(&format!("Use: 2 x 12 V {s} Ah batteries (verify panel charger limit)")),
        None => host.push_output("Required capacity is above 100 Ah: use an external charger / battery cabinet."),
    }
}

fn vdrop_calc(host: &mut dyn HostApi) {
    let devs = collect_devices(host);
    let mut circuits: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    for p in &devs {
        let is_nac = p.data.device_type().is_some_and(|d| d.category == Category::Notification);
        if is_nac && !p.data.circuit.is_empty() {
            let e = circuits.entry(p.data.circuit.clone()).or_default();
            e.0 += 1;
            e.1 += p.data.alarm_ma;
        }
    }
    if circuits.is_empty() {
        host.push_info("Fire Alarm: no notification devices on a NAC yet. Place horns/strobes first.");
        return;
    }
    let names: Vec<String> = circuits.keys().cloned().collect();
    let dialog = DialogDef::new("NAC Voltage Drop")
        .with_row(DialogRow::single(DialogControl::InfoText {
            text: {
                let wires = wire_lengths_ft(host);
                circuits
                    .iter()
                    .map(|(k, (n, ma))| match wires.get(k) {
                        Some(ft) => format!("{k}: {n} devices, {:.2} A, {:.0} ft drawn", ma / 1000.0, ft),
                        None => format!("{k}: {n} devices, {:.2} A, no wire drawn", ma / 1000.0),
                    })
                    .collect::<Vec<_>>()
                    .join("   ")
            },
            align_h: None,
        }))
        .with_row(DialogRow::new(vec![
            DialogControl::Choice { id: "ckt".into(), label: "Circuit".into(), options: names.clone(), default_index: 0 },
            DialogControl::Number { id: "len".into(), label: "One-way length ft (0 = drawn wire)".into(), default: 0.0, min: Some(0.0), max: None, step: Some(10.0) },
            DialogControl::Choice { id: "awg".into(), label: "Wire".into(), options: vec!["18".into(), "16".into(), "14".into(), "12".into(), "10".into()], default_index: 2 },
        ]))
        .with_row(DialogRow::new(vec![
            DialogControl::Number { id: "src".into(), label: "Panel volts (end of battery)".into(), default: 20.4, min: Some(1.0), max: Some(30.0), step: Some(0.1) },
            DialogControl::Number { id: "vmin".into(), label: "Min device volts".into(), default: 16.0, min: Some(1.0), max: Some(30.0), step: Some(0.1) },
        ]))
        .submit_label("Calculate");
    open_dialog(host, "VDROP", dialog);
}

fn nac_circuits(host: &mut dyn HostApi) -> BTreeMap<String, (usize, f64)> {
    let mut circuits: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    for p in &collect_devices(host) {
        let is_nac = p.data.device_type().is_some_and(|d| d.category == Category::Notification);
        if is_nac && !p.data.circuit.is_empty() {
            let e = circuits.entry(p.data.circuit.clone()).or_default();
            e.0 += 1;
            e.1 += p.data.alarm_ma;
        }
    }
    circuits
}

fn vdrop_apply(host: &mut dyn HostApi, v: &HashMap<String, DialogValue>) {
    let circuits = nac_circuits(host);
    let Some(first) = circuits.keys().next().cloned() else { return };
    let ckt = text(v.get("ckt")).filter(|c| circuits.contains_key(c)).unwrap_or(first);
    let Some((n, ma)) = circuits.get(&ckt).copied() else { return };
    let input = calc::VdropInput {
        amps: ma / 1000.0,
        length_ft: match num(v.get("len")).filter(|l| *l > 0.0) {
            Some(l) => l,
            None => match wire_lengths_ft(host).get(&ckt).copied() {
                Some(l) => l,
                None => {
                    host.push_error(&format!("Fire Alarm: no wire drawn for {ckt}. Draw it with Wire Circuit or type a length."));
                    return;
                }
            },
        },
        awg: text(v.get("awg")).and_then(|t| t.parse().ok()).unwrap_or(14),
        source_volts: num(v.get("src")).unwrap_or(20.4),
        min_volts: num(v.get("vmin")).unwrap_or(16.0),
    };
    let Some(r) = calc::voltage_drop(input) else { return };
    host.push_output(&format!("── Voltage drop: {ckt} ──"));
    host.push_output(&format!("{n} devices, {:.3} A, {} ft of {} AWG (load lumped at end)", input.amps, input.length_ft, input.awg));
    host.push_output(&format!("Drop {:.2} V ({:.1}%), end of line {:.2} V, minimum {:.1} V", r.drop_volts, r.drop_percent, r.end_volts, input.min_volts));
    host.push_output(&format!("Max length at this load: {:.0} ft", r.max_length_ft));
    if r.ok {
        host.push_output("PASS");
    } else {
        host.push_error(&format!("FAIL: {ckt} is below {:.1} V at the last device. Use heavier wire, shorten the run, or split the circuit.", input.min_volts));
    }
}


// ───────────────────────────── geometry helpers ─────────────────────────────

/// Insertion point and rotation (radians) of every placed device.
fn device_positions(host: &mut dyn HostApi, devs: &[Placed]) -> Vec<Option<(f64, f64, f64)>> {
    let handles: Vec<Handle> = devs.iter().map(|p| p.handle).collect();
    host.entities_batch(&handles)
        .unwrap_or_default()
        .into_iter()
        .map(|e| match e {
            Some(EntityType::Insert(i)) => Some((i.insert_point.x, i.insert_point.y, i.rotation)),
            _ => None,
        })
        .chain(std::iter::repeat(None))
        .take(devs.len())
        .collect()
}

fn on_layer<T>(mut e: T, layer: &str, set: impl FnOnce(&mut T, String)) -> T {
    set(&mut e, layer.to_string());
    e
}

fn poly(points: &[(f64, f64)], closed: bool, layer: &str) -> EntityType {
    let mut p = LwPolyline::from_points(points.iter().map(|(x, y)| Vector2::new(*x, *y)).collect());
    if closed {
        p.close();
    }
    p.common.layer = layer.to_string();
    EntityType::LwPolyline(p)
}

fn label(textv: &str, x: f64, y: f64, h: f64, layer: &str) -> EntityType {
    let t = on_layer(Text::with_value(textv, Vector3::new(x, y, 0.0)).with_height(h), layer, |t, l| t.common.layer = l);
    EntityType::Text(t)
}

fn tag_record(tag: &str) -> ExtendedDataRecord {
    let mut r = ExtendedDataRecord::new(APP);
    r.add_value(XDataValue::String(tag.to_string()));
    r
}

/// Every entity carrying a FIREALARM record whose first string starts with `prefix`.
fn tagged_entities(host: &mut dyn HostApi, layer: &str, prefix: &str) -> Vec<(Handle, String, EntityType)> {
    let mut out = Vec::new();
    let mut offset = 0u64;
    loop {
        let q = format!(r#"{{"layer":"{layer}","xdata_app":"{APP}","offset":{offset},"limit":2000}}"#);
        let Ok(page) = host.query_entities(&q) else { break };
        for e in page.entities {
            let h = e.common().handle;
            let tag = host.read_record(h, APP).and_then(|r| {
                r.values.iter().find_map(|v| match v {
                    XDataValue::String(s) if s.starts_with(prefix) => Some(s.clone()),
                    _ => None,
                })
            });
            if let Some(tag) = tag {
                out.push((h, tag, e));
            }
        }
        match page.next_offset {
            Some(n) if n > offset => offset = n,
            _ => break,
        }
    }
    out
}

fn polyline_length(e: &EntityType) -> f64 {
    match e {
        EntityType::LwPolyline(p) => p
            .vertices
            .windows(2)
            .map(|w| ((w[1].location.x - w[0].location.x).powi(2) + (w[1].location.y - w[0].location.y).powi(2)).sqrt())
            .sum(),
        _ => 0.0,
    }
}

/// Drawn wire length per circuit, in feet.
fn wire_lengths_ft(host: &mut dyn HostApi) -> BTreeMap<String, f64> {
    let upf = settings().lock().unwrap().units_per_ft;
    let mut out = BTreeMap::new();
    for (_, tag, e) in tagged_entities(host, WIRING_LAYER, WIRE_PREFIX) {
        let ckt = tag.trim_start_matches(WIRE_PREFIX).to_string();
        *out.entry(ckt).or_insert(0.0) += polyline_length(&e) / upf;
    }
    out
}

// ───────────────────────────── coverage ─────────────────────────────

fn coverage(host: &mut dyn HostApi) {
    if !ensure_setup(host) {
        return;
    }
    let s = settings().lock().unwrap().clone();
    let upf = s.units_per_ft;
    let old = tagged_entities(host, COVERAGE_LAYER, COVERAGE_TAG);
    host.begin_undo_group("Fire alarm coverage");
    host.push_undo("Fire alarm coverage");
    for (h, _, _) in &old {
        host.remove_entity(*h);
    }
    let devs = collect_devices(host);
    let pos = device_positions(host, &devs);
    let mut ents: Vec<EntityType> = Vec::new();
    let (mut n_det, mut n_strobe, mut missing) = (0, 0, Vec::new());
    for (p, at) in devs.iter().zip(pos) {
        let Some((x, y, rot)) = at else { continue };
        let key = p.data.key.as_str();
        let text_h = 4.0 * s.scale * upf / 12.0;
        match key {
            "SMOKE" | "HEAT" => {
                let spacing = if key == "SMOKE" { s.smoke_spacing_ft } else { s.heat_spacing_ft };
                let r = coverage::detector_radius_ft(spacing) * upf;
                let mut c = Circle::from_center_radius(Vector3::new(x, y, 0.0), r);
                c.common.layer = COVERAGE_LAYER.to_string();
                ents.push(EntityType::Circle(c));
                n_det += 1;
            }
            "HS-WALL" | "S-WALL" | "SS-WALL" | "HS-CLG" | "S-CLG" | "SS-CLG" => {
                let ceiling = key.ends_with("CLG");
                let room = if ceiling { coverage::ceiling_strobe_room_ft(p.data.candela) } else { coverage::wall_strobe_room_ft(p.data.candela) };
                let Some(side) = room else {
                    missing.push(format!("{} {} cd", p.data.device_type().map(|d| d.label).unwrap_or(key), p.data.candela));
                    continue;
                };
                let l = side * upf;
                // Wall strobe sits at the middle of one wall; the room extends
                // toward the symbol's bottom (local -Y). Ceiling strobe is centered.
                let local: [(f64, f64); 4] = if ceiling {
                    [(-l / 2.0, -l / 2.0), (l / 2.0, -l / 2.0), (l / 2.0, l / 2.0), (-l / 2.0, l / 2.0)]
                } else {
                    [(-l / 2.0, 0.0), (l / 2.0, 0.0), (l / 2.0, -l), (-l / 2.0, -l)]
                };
                let (sn, cs) = rot.sin_cos();
                let pts: Vec<(f64, f64)> = local.iter().map(|(lx, ly)| (x + lx * cs - ly * sn, y + lx * sn + ly * cs)).collect();
                ents.push(poly(&pts, true, COVERAGE_LAYER));
                let (cx, cy) = if ceiling { (x, y) } else { (x + (l / 2.0) * sn, y - (l / 2.0) * cs) };
                ents.push(label(&format!("{} cd {:.0}'x{:.0}'", p.data.candela, side, side), cx - 3.0 * text_h, cy, text_h, COVERAGE_LAYER));
                n_strobe += 1;
            }
            _ => {}
        }
    }
    let handles = host.add_entities(ents);
    for h in handles {
        host.write_record(h, tag_record(COVERAGE_TAG));
    }
    host.end_undo_group();
    host.set_dirty();
    host.push_output("── Coverage ──");
    host.push_output(&format!(
        "{n_det} detector circles (smoke {:.0} ft spacing = {:.0} ft radius, heat {:.0} ft = {:.0} ft radius)",
        s.smoke_spacing_ft, coverage::detector_radius_ft(s.smoke_spacing_ft), s.heat_spacing_ft, coverage::detector_radius_ft(s.heat_spacing_ft)
    ));
    host.push_output(&format!("{n_strobe} strobe rooms (wall: room extends below the symbol; ceiling: centered, 10 ft ceiling)"));
    for m in missing {
        host.push_error(&format!("No prescriptive room size for {m}"));
    }
    host.push_output("Coverage is on layer FA-COVERAGE. Run Coverage again after moving devices. Verify against your NFPA 72 edition.");
}

// ───────────────────────────── wiring ─────────────────────────────

struct WireCmd {
    /// (handle value, x, y, circuit) of every device that can be picked.
    devices: Vec<(u64, f64, f64, String)>,
    snap: f64,
    points: Vec<(f64, f64)>,
    picked: Vec<u64>,
    circuit: Option<String>,
}

impl WireCmd {
    fn nearest(&self, x: f64, y: f64) -> Option<&(u64, f64, f64, String)> {
        self.devices
            .iter()
            .map(|d| (d, (d.1 - x).hypot(d.2 - y)))
            .filter(|(_, dist)| *dist <= self.snap)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(d, _)| d)
    }
}

impl InteractiveCommand for WireCmd {
    fn prompt(&self) -> String {
        match &self.circuit {
            None => "Wire circuit: click the first device (or the panel), Enter to finish".into(),
            Some(c) => format!("Wire {c}: click the next device or a bend point, Enter to finish ({} devices)", self.picked.len()),
        }
    }
    fn on_point(&mut self, pt: [f64; 3]) -> CommandStep {
        let hit = self.nearest(pt[0], pt[1]).cloned();
        match hit {
            Some((h, x, y, ckt)) => {
                if self.circuit.is_none() && !ckt.is_empty() {
                    self.circuit = Some(ckt);
                }
                if !self.picked.contains(&h) {
                    self.picked.push(h);
                }
                self.points.push((x, y));
            }
            None => self.points.push((pt[0], pt[1])),
        }
        CommandStep::NeedPoint
    }
    fn on_cursor_move(&mut self, pt: [f64; 3]) -> Vec<PreviewWire> {
        let mut pts: Vec<[f64; 3]> = self.points.iter().map(|(x, y)| [*x, *y, 0.0]).collect();
        if pts.is_empty() {
            return Vec::new();
        }
        pts.push(pt);
        vec![PreviewWire::Polyline { points: pts, color: Some([0.2, 0.9, 0.2, 1.0]) }]
    }
    fn on_enter(&mut self) -> CommandStep {
        if self.points.len() < 2 {
            return CommandStep::Done;
        }
        let ckt = self.circuit.clone().unwrap_or_else(|| settings().lock().unwrap().nac.clone());
        let hs: Vec<String> = self.picked.iter().map(|h| format!("{h:X}")).collect();
        let pts: Vec<String> = self.points.iter().map(|(x, y)| format!("{x},{y}")).collect();
        CommandStep::Dispatch(format!("FA_WIREDONE|{}|{}|{}", enc(&ckt), hs.join(","), pts.join(";")))
    }
}

fn start_wire(host: &mut dyn HostApi) {
    if !ensure_setup(host) {
        return;
    }
    let s = settings().lock().unwrap().clone();
    let devs = collect_devices(host);
    let pos = device_positions(host, &devs);
    let devices = devs
        .iter()
        .zip(pos)
        .filter_map(|(p, at)| at.map(|(x, y, _)| (p.handle.value(), x, y, p.data.circuit.clone())))
        .collect();
    host.start_interactive(Box::new(WireCmd {
        devices,
        snap: 12.0 * s.scale * s.units_per_ft / 12.0,
        points: Vec::new(),
        picked: Vec::new(),
        circuit: None,
    }));
}

fn wire_done(host: &mut dyn HostApi, body: &str) {
    let mut parts = body.splitn(3, '|');
    let ckt = dec(parts.next().unwrap_or("")).to_uppercase();
    let handles: Vec<Handle> = parts
        .next()
        .unwrap_or("")
        .split(',')
        .filter_map(|h| u64::from_str_radix(h.trim(), 16).ok())
        .map(Handle::new)
        .collect();
    let pts: Vec<(f64, f64)> = parts
        .next()
        .unwrap_or("")
        .split(';')
        .filter_map(|p| p.split_once(',').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))))
        .collect();
    if pts.len() < 2 || ckt.is_empty() {
        return;
    }
    let upf = settings().lock().unwrap().units_per_ft;
    host.begin_undo_group(&format!("Wire {ckt}"));
    host.push_undo(&format!("Wire {ckt}"));
    let h = host.add_entity(poly(&pts, false, WIRING_LAYER));
    host.write_record(h, tag_record(&format!("{WIRE_PREFIX}{ckt}")));
    // Notification devices on a NAC take the wired circuit name.
    let mut moved = 0;
    for dh in &handles {
        if let Some(mut d) = host.read_record(*dh, APP).and_then(DeviceData::from_record) {
            let is_nac = d.device_type().is_some_and(|t| t.category == Category::Notification);
            if is_nac && d.circuit != ckt {
                d.circuit = ckt.clone();
                host.write_record(*dh, d.to_record());
                moved += 1;
            }
        }
    }
    host.end_undo_group();
    host.set_dirty();
    let len_ft: f64 = pts.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum::<f64>() / upf;
    host.push_output(&format!("{ckt}: wired {} devices, {:.0} ft drawn{}", handles.len(), len_ft,
        if moved > 0 { format!(", {moved} moved onto {ckt}") } else { String::new() }));
}

// ───────────────────────────── riser ─────────────────────────────

struct PickPointCmd {
    prompt: &'static str,
    command: &'static str,
}

impl InteractiveCommand for PickPointCmd {
    fn prompt(&self) -> String {
        self.prompt.to_string()
    }
    fn on_point(&mut self, pt: [f64; 3]) -> CommandStep {
        CommandStep::Dispatch(format!("{}|{}|{}", self.command, pt[0], pt[1]))
    }
}

fn riser_at(host: &mut dyn HostApi, x0: f64, y0: f64) {
    if !ensure_setup(host) {
        return;
    }
    let s = settings().lock().unwrap().clone();
    let devs = collect_devices(host);
    // circuit -> (device key, candela) -> count, SLC circuits first.
    let mut circuits: BTreeMap<(u8, String), BTreeMap<(usize, u32), usize>> = BTreeMap::new();
    let mut panels = 0;
    for p in &devs {
        let Some(dt) = p.data.device_type() else { continue };
        if dt.category == Category::Panel && dt.key == "FACP" {
            panels += 1;
            continue;
        }
        let ckt = if p.data.circuit.is_empty() { "OTHER".to_string() } else { p.data.circuit.clone() };
        let order = if ckt.starts_with("SLC") { 0 } else if ckt == "OTHER" { 2 } else { 1 };
        let idx = DEVICES.iter().position(|d| d.key == dt.key).unwrap_or(0);
        *circuits.entry((order, ckt)).or_default().entry((idx, p.data.candela)).or_default() += 1;
    }
    if circuits.is_empty() {
        host.push_info("Fire Alarm: no devices to put on a riser yet.");
        return;
    }
    let u = s.scale * s.units_per_ft / 12.0; // one symbol width = 12 * u
    let th = 4.0 * u;
    let dx = 30.0 * u;
    let dy = 36.0 * u;
    let mut ents: Vec<EntityType> = Vec::new();
    // Panel
    if let Some(facp) = catalog::by_key("FACP") {
        let mut ins = build_insert(host, facp, Vector3::new(x0, y0, 0.0), s.scale, &[]);
        ins.common.layer = RISER_LAYER.to_string();
        ents.push(EntityType::Insert(ins));
    }
    ents.push(label("FIRE ALARM RISER DIAGRAM", x0 - 13.0 * u, y0 + 16.0 * u, th * 1.5, RISER_LAYER));
    let bus_x = x0 + 13.0 * u;
    let n = circuits.len();
    let bottom = y0 - dy * n as f64;
    ents.push(EntityType::Line(on_layer(Line::from_coords(bus_x, y0, 0.0, bus_x, bottom, 0.0), RISER_LAYER, |l, lay| l.common.layer = lay)));
    for (row, ((_, ckt), items)) in circuits.iter().enumerate() {
        let y = y0 - dy * (row as f64 + 1.0);
        let end_x = bus_x + dx * (items.len() as f64 + 1.5);
        ents.push(EntityType::Line(on_layer(Line::from_coords(bus_x, y, 0.0, end_x, y, 0.0), RISER_LAYER, |l, lay| l.common.layer = lay)));
        let total: usize = items.values().sum();
        ents.push(label(&format!("{ckt} ({total})"), bus_x + 2.0 * u, y + 2.0 * u, th, RISER_LAYER));
        for (i, ((idx, cd), count)) in items.iter().enumerate() {
            let dt = &DEVICES[*idx];
            let cx = bus_x + dx * (i as f64 + 1.5);
            let cy = y + 12.0 * u;
            let attrs = [("CD", if dt.strobe { cd.to_string() } else { String::new() }), ("ADDR", String::new()), ("WW", String::new())];
            let mut ins = build_insert(host, dt, Vector3::new(cx, cy, 0.0), s.scale, &attrs);
            ins.common.layer = RISER_LAYER.to_string();
            ins.attributes.iter_mut().for_each(|a| a.common.layer = RISER_LAYER.to_string());
            ents.push(EntityType::Insert(ins));
            ents.push(EntityType::Line(on_layer(Line::from_coords(cx, y, 0.0, cx, cy - 7.0 * u, 0.0), RISER_LAYER, |l, lay| l.common.layer = lay)));
            ents.push(label(&format!("x{count}"), cx + 8.0 * u, cy - 4.0 * u, th, RISER_LAYER));
        }
    }
    if panels == 0 {
        ents.push(label("(panel not placed on plan)", x0 - 13.0 * u, y0 - 10.0 * u, th * 0.8, RISER_LAYER));
    }
    host.begin_undo_group("Fire alarm riser");
    host.push_undo("Fire alarm riser");
    host.add_entities(ents);
    host.end_undo_group();
    host.set_dirty();
    host.push_output(&format!("Riser drawn: {n} circuits, {} devices. It is a snapshot; draw it again after changes.", devs.len()));
}

// ───────────────────────────── ribbon ─────────────────────────────

fn tool(id: &'static str, label: &'static str, glyph: &'static str, cmd: &'static str, large: bool) -> RibbonItem {
    let t = ToolDef { id, label, icon: IconKind::Glyph(glyph), event: ModuleEvent::Command(cmd.to_string()) };
    if large { RibbonItem::LargeTool(t) } else { RibbonItem::LabeledTool(t) }
}

fn device_tools(cat: &[Category]) -> Vec<RibbonItem> {
    DEVICES
        .iter()
        .filter(|d| cat.contains(&d.category))
        .map(|d| {
            let id: &'static str = Box::leak(format!("FA_{}", d.key).into_boxed_str());
            let glyph = match d.category {
                Category::Initiating => "◇",
                Category::Notification => "▽",
                Category::Module => "⬡",
                Category::Panel => "▭",
                Category::Misc => "✕",
            };
            tool(id, d.short, glyph, id, false)
        })
        .collect()
}

struct FireAlarmModule;

impl CadModule for FireAlarmModule {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }
    fn title(&self) -> &'static str {
        "Fire Alarm"
    }
    fn ribbon_groups(&self) -> &[RibbonGroup] {
        static GROUPS: OnceLock<Vec<RibbonGroup>> = OnceLock::new();
        GROUPS.get_or_init(|| {
            vec![
                RibbonGroup { title: "Setup", tools: vec![
                    tool("FA_SETTINGS", "Settings", "⚙", "FA_SETTINGS", true),
                    tool("FA_SETUP", "Load Symbols", "⬇", "FA_SETUP", true),
                ]},
                RibbonGroup { title: "Initiating", tools: device_tools(&[Category::Initiating]) },
                RibbonGroup { title: "Notification", tools: device_tools(&[Category::Notification]) },
                RibbonGroup { title: "Modules & Panels", tools: device_tools(&[Category::Module, Category::Panel, Category::Misc]) },
                RibbonGroup { title: "Design", tools: vec![
                    tool("FA_COVERAGE", "Coverage", "◎", "FA_COVERAGE", true),
                    tool("FA_WIRE", "Wire Circuit", "∿", "FA_WIRE", true),
                    tool("FA_RISER", "Riser", "≡", "FA_RISER", true),
                ]},
                RibbonGroup { title: "Reports & Calcs", tools: vec![
                    tool("FA_COUNT", "Device Count", "#", "FA_COUNT", true),
                    RibbonItem::LargeTool(ToolDef {
                        id: "FA_EXPORT_PICK", label: "Export CSV", icon: IconKind::Glyph("⇩"),
                        event: ModuleEvent::PluginFileDialog {
                            command: "FA_EXPORT".into(), title: "Export device list".into(),
                            filter_name: "CSV".into(), extensions: vec!["csv".into()],
                        },
                    }),
                    tool("FA_BATTERY", "Battery Calc", "⚡", "FA_BATTERY", true),
                    tool("FA_VDROP", "Voltage Drop", "∆", "FA_VDROP", true),
                ]},
            ]
        })
    }
}

// ───────────────────────────── plugin ─────────────────────────────

fn entity_classes() -> &'static EntityClasses {
    static EC: OnceLock<EntityClasses> = OnceLock::new();
    EC.get_or_init(|| {
        let labels: Vec<(&'static str, &'static str)> = DEVICES.iter().map(|d| (d.key, d.label)).collect();
        EntityClasses { app_id: APP, class_prefix: CLASS_PREFIX, labels: Box::leak(labels.into_boxed_slice()) }
    })
}

struct FireAlarmPlugin;

impl BuiltinPlugin for FireAlarmPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }
    fn ribbon(&self) -> Box<dyn CadModule> {
        Box::new(FireAlarmModule)
    }
    fn entity_classes(&self) -> Option<&'static EntityClasses> {
        Some(entity_classes())
    }
    fn dispatch(&self, host: &mut dyn HostApi, cmd: &str) -> bool {
        let trimmed = cmd.trim();
        let upper = trimmed.to_ascii_uppercase();
        if let Some(rest) = upper.strip_prefix("FA_PUT|") {
            let parts: Vec<&str> = rest.split('|').collect();
            if let [key, x, y] = parts.as_slice() {
                if let (Ok(x), Ok(y)) = (x.parse(), y.parse()) {
                    put_device(host, key, x, y);
                    return true;
                }
            }
            host.push_error("Fire Alarm: bad placement data");
            return true;
        }
        if let Some(body) = trimmed.strip_prefix("FA_WIREDONE|").or_else(|| trimmed.strip_prefix("fa_wiredone|")) {
            wire_done(host, body);
            return true;
        }
        if let Some(rest) = upper.strip_prefix("FA_RISERAT|") {
            if let Some((x, y)) = rest.split_once('|').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))) {
                riser_at(host, x, y);
            }
            return true;
        }
        if upper.starts_with("FA_DLG|") {
            let mut parts = trimmed.splitn(3, '|');
            let _ = parts.next();
            let kind = parts.next().unwrap_or("").to_ascii_uppercase();
            let v = parse_dlg(parts.next().unwrap_or(""));
            match kind.as_str() {
                "SETTINGS" => settings_apply(host, &v),
                "BATTERY" => battery_apply(host, &v),
                "VDROP" => vdrop_apply(host, &v),
                _ => {}
            }
            return true;
        }
        if upper.starts_with("FA_EXPORT ") {
            let path = trimmed["FA_EXPORT ".len()..].trim().trim_matches('"');
            export_csv(host, path);
            return true;
        }
        match upper.as_str() {
            "FA_SETUP" => {
                if ensure_setup(host) {
                    host.push_info("Fire Alarm: ready. Pick a device on the Fire Alarm tab to place it.");
                }
                true
            }
            "FA_SETTINGS" => { settings_dialog(host); true }
            "FA_COUNT" => { device_count(host); true }
            "FA_BATTERY" => { battery_calc(host); true }
            "FA_VDROP" => { vdrop_calc(host); true }
            "FA_COVERAGE" => { coverage(host); true }
            "FA_WIRE" => { start_wire(host); true }
            "FA_RISER" => {
                host.start_interactive(Box::new(PickPointCmd { prompt: "Pick the top-left point for the riser diagram", command: "FA_RISERAT" }));
                true
            }
            "FA_EXPORT" => {
                host.push_info("Use the Export CSV button, or type FA_EXPORT followed by a file path.");
                true
            }
            other => {
                if let Some(dev) = other.strip_prefix("FA_").and_then(catalog::by_key) {
                    start_place(host, dev);
                    true
                } else {
                    false
                }
            }
        }
    }
}

export_plugin!(FireAlarmPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_encoding_roundtrip() {
        for s in ["NAC-1", "1/4", "a b;c=d|e%", "Ünïcode 15"] {
            assert_eq!(dec(&enc(s)), s);
        }
        let v = parse_dlg(&format!("{}={};{}={}", enc("nac"), enc("nac 2"), enc("cd"), enc("75")));
        assert_eq!(text(v.get("nac")).as_deref(), Some("nac 2"));
        assert_eq!(num(v.get("cd")), Some(75.0));
    }
}
