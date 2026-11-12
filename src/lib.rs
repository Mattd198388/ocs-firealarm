//! Fire Alarm design tools for Open CAD Studio (AlarmCAD-style).
//!
//! Phase 1-3: NFPA 170 device placement with auto-addressing, device data on
//! every symbol (XDATA app `FIREALARM`), device counts / CSV export, battery
//! calcs and NAC voltage drop.

pub mod calc;
pub mod catalog;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};

use catalog::{Category, DeviceType, DEVICES};
use ocs_plugin_api::dialog::{DialogControl, DialogDef, DialogResponse, DialogRow, DialogValue};
use ocs_plugin_api::export_plugin;
use ocs_plugin_api::host::acadrust::entities::{AttributeEntity, Insert};
use ocs_plugin_api::host::acadrust::types::{Color, Vector3};
use ocs_plugin_api::host::{
    BuiltinPlugin, CommandRequest, CommandStep, EntityType, ExtendedDataRecord, Handle, HostApi,
    InteractiveCommand, LayerConfig, XDataValue,
};
use ocs_plugin_api::manifest::{ApiVersion, EntityClasses, PluginManifest};
use ocs_plugin_api::ribbon::{CadModule, IconKind, ModuleEvent, RibbonGroup, RibbonItem, ToolDef};

const PLUGIN_ID: &str = "matt.firealarm";
const APP: &str = "FIREALARM";
const CLASS_PREFIX: &str = "FA-DEVICE:";
const DEVICE_LAYER: &str = "FA-DEVICES";
const LIBRARY_FILE: &str = "fa_blocks.dxf";

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
}

impl Default for Settings {
    fn default() -> Self {
        Self { scale: 1.0, loop_no: 1, next_addr: None, candela: 15, watts: "1/4".into(), nac: "NAC-1".into() }
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
        ("FA-WIRING", 3, "Fire alarm wiring"),
        ("FA-TEXT", 7, "Fire alarm notes and tags"),
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
    let dialog = DialogDef::new("Fire Alarm Settings")
        .with_row(DialogRow::single(DialogControl::InfoText {
            text: "Symbols are drawn 12 in. across at scale 1. Use a bigger scale for larger symbols.".into(),
            align_h: None,
        }))
        .with_row(DialogRow::single(DialogControl::Number {
            id: "scale".into(), label: "Symbol scale".into(), default: s.scale, min: Some(0.01), max: Some(1000.0), step: Some(0.25),
        }))
        .with_row(DialogRow::new(vec![
            DialogControl::Number { id: "loop".into(), label: "SLC loop".into(), default: s.loop_no as f64, min: Some(1.0), max: Some(99.0), step: Some(1.0) },
            DialogControl::Number { id: "next".into(), label: "Next address".into(), default: next as f64, min: Some(1.0), max: Some(999.0), step: Some(1.0) },
        ]))
        .with_row(DialogRow::new(vec![
            DialogControl::Choice {
                id: "cd".into(), label: "Strobe candela".into(),
                options: catalog::CANDELAS.iter().map(|c| format!("{c}")).collect(), default_index: cd_idx,
            },
            DialogControl::Text { id: "watts".into(), label: "Speaker tap (W)".into(), default: s.watts.clone() },
        ]))
        .with_row(DialogRow::single(DialogControl::Text { id: "nac".into(), label: "NAC circuit for new notification devices".into(), default: s.nac.clone() }))
        .submit_label("Save");
    open_dialog(host, "SETTINGS", dialog);
}

fn settings_apply(host: &mut dyn HostApi, v: &HashMap<String, DialogValue>) {
    {
        let mut s = settings().lock().unwrap();
        if let Some(x) = num(v.get("scale")).filter(|x| *x > 0.0) { s.scale = x; }
        let new_loop = num(v.get("loop")).map(|x| x.round().max(1.0) as u32).unwrap_or(s.loop_no);
        let new_next = num(v.get("next")).map(|x| x.round().max(1.0) as u32);
        s.loop_no = new_loop;
        s.next_addr = new_next;
        if let Some(t) = text(v.get("cd")).and_then(|t| t.parse().ok()) { s.candela = t; }
        if let Some(t) = text(v.get("watts")).filter(|t| !t.is_empty()) { s.watts = t; }
        if let Some(t) = text(v.get("nac")).filter(|t| !t.is_empty()) { s.nac = t.to_uppercase(); }
        let msg = format!(
            "Fire Alarm settings: scale {}, loop {}, next address {}, {} cd, {} W, {}",
            s.scale, s.loop_no, s.next_addr.unwrap_or(1), s.candela, s.watts, s.nac
        );
        drop(s);
        host.push_info(&msg);
    }
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
            text: circuits.iter().map(|(k, (n, ma))| format!("{k}: {n} devices, {:.2} A", ma / 1000.0)).collect::<Vec<_>>().join("   "),
            align_h: None,
        }))
        .with_row(DialogRow::new(vec![
            DialogControl::Choice { id: "ckt".into(), label: "Circuit".into(), options: names.clone(), default_index: 0 },
            DialogControl::Number { id: "len".into(), label: "One-way length (ft)".into(), default: 200.0, min: Some(1.0), max: None, step: Some(10.0) },
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
        length_ft: num(v.get("len")).unwrap_or(200.0),
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
