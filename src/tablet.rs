//! Pen pressure from a drawing tablet (ZBrush: Brush > Tablet Pressure).
//!
//! winit reports no tablet pressure on Linux, so on X11 Klay opens a second connection to
//! the X server that only listens: XInput2 raw events on the root window, from every
//! device. A device with an "Abs Pressure" axis (the label the libinput, wacom and evdev
//! drivers give a pen's pressure) is a pen; the pressure of the device that last moved the
//! pointer is the pressure of the next dab. A mouse, or any device without that axis,
//! means no pressure: full size and strength.
//!
//! The libinput driver adds a pen's device the first time the pen comes near the tablet,
//! so the device list is read again whenever the server reports a device change. A thread
//! owns the connection and waits on it, so the events (sent for every pointer move on the
//! screen) are read as they come even while Klay is minimised.

/// A device with a pressure axis.
#[derive(Clone, Debug, PartialEq)]
pub struct Pen {
    pub id: u16,
    pub name: String,
    /// Valuator number of the pressure axis and its range.
    axis: u16,
    min: f64,
    max: f64,
    /// Its last pressure, 0..=1 (drivers only send the axes that changed).
    last: f32,
}

impl Pen {
    pub fn new(id: u16, name: &str, axis: u16, min: f64, max: f64) -> Self {
        Self {
            id,
            name: name.to_string(),
            axis,
            min,
            max,
            last: 0.0,
        }
    }
}

/// What the raw events say, kept apart from the connection so it can be tested.
#[derive(Default, Debug)]
pub struct Pressure {
    pub pens: Vec<Pen>,
    /// Pen that last moved the pointer; `None` after a device without pressure moved it.
    active: Option<u16>,
    /// Raw events seen, and the device of the last one (for `--selftest-pressure`).
    pub events: u64,
    pub last_source: Option<u16>,
}

impl Pressure {
    /// A raw motion or button event from device `source`: `mask` says which axes have a
    /// value in `values` (one per set bit, in axis order), each a 32.32 fixed-point pair.
    pub fn raw(&mut self, source: u16, mask: &[u32], values: &[(i32, u32)]) {
        self.events += 1;
        self.last_source = Some(source);
        let Some(pen) = self.pens.iter_mut().find(|p| p.id == source) else {
            self.active = None;
            return;
        };
        if let Some(v) = axis_value(mask, values, pen.axis) {
            let span = pen.max - pen.min;
            pen.last = if span > 0.0 { ((v - pen.min) / span).clamp(0.0, 1.0) as f32 } else { 1.0 };
        }
        self.active = Some(pen.id);
    }

    /// Pressure of the pen in use, 0..=1, or `None` when the pointer is moved by a device
    /// without pressure.
    pub fn current(&self) -> Option<f32> {
        let id = self.active?;
        self.pens.iter().find(|p| p.id == id).map(|p| p.last)
    }

    /// The pen in use, if any.
    pub fn active_pen(&self) -> Option<&Pen> {
        let id = self.active?;
        self.pens.iter().find(|p| p.id == id)
    }

    /// Replace the device list (after a hot-plug), keeping each known pen's last pressure.
    pub fn set_pens(&mut self, mut pens: Vec<Pen>) {
        for p in &mut pens {
            if let Some(old) = self.pens.iter().find(|o| o.id == p.id) {
                p.last = old.last;
            }
        }
        if self.active.is_some_and(|id| !pens.iter().any(|p| p.id == id)) {
            self.active = None;
        }
        self.pens = pens;
    }
}

/// Value of `axis` in a raw event, if it carries one.
fn axis_value(mask: &[u32], values: &[(i32, u32)], axis: u16) -> Option<f64> {
    let (word, bit) = (axis as usize / 32, axis as u32 % 32);
    let w = *mask.get(word)?;
    if w & (1 << bit) == 0 {
        return None;
    }
    let before = mask[..word].iter().map(|w| w.count_ones()).sum::<u32>() + (w & ((1u32 << bit) - 1)).count_ones();
    values.get(before as usize).map(|&(i, f)| i as f64 + f as f64 / 4_294_967_296.0)
}

/// A device's pressure axis: the valuator labelled "Abs Pressure" (`label` is that atom,
/// 0 when the server has none), or, for a device named like a pen whose driver leaves its
/// axes unlabelled, valuator 2 (x, y, pressure). `valuators` are `(number, label, min,
/// max)`.
fn pressure_axis(name: &str, valuators: &[(u16, u32, f64, f64)], label: u32) -> Option<(u16, f64, f64)> {
    if label != 0 {
        if let Some(&(n, _, min, max)) = valuators.iter().find(|v| v.1 == label && v.3 > v.2) {
            return Some((n, min, max));
        }
    }
    let lower = name.to_ascii_lowercase();
    let pen_like = ["pen", "stylus"].iter().any(|w| lower.contains(w));
    if pen_like && valuators.iter().all(|v| v.1 == 0) {
        if let Some(&(n, _, min, max)) = valuators.iter().find(|v| v.0 == 2 && v.3 > v.2) {
            return Some((n, min, max));
        }
    }
    None
}

#[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
mod x11 {
    use super::{Pen, Pressure, pressure_axis};
    use x11rb::connection::{Connection, RequestConnection as _};
    use x11rb::protocol::Event;
    use x11rb::protocol::xinput::{self, ConnectionExt as _};
    use x11rb::protocol::xproto::ConnectionExt as _;
    use x11rb::rust_connection::RustConnection;
    use std::sync::{Arc, Mutex, MutexGuard};

    /// XIAllDevices.
    const ALL_DEVICES: u16 = 0;

    /// The pressure state, kept up to date by a thread that owns the X connection and
    /// blocks on it, so events never pile up even while Klay is not drawing.
    pub struct Tablet {
        state: Arc<Mutex<Pressure>>,
        error: Arc<Mutex<Option<String>>>,
    }

    fn fp(v: xinput::Fp3232) -> f64 {
        v.integral as f64 + v.frac as f64 / 4_294_967_296.0
    }

    fn err(e: impl std::fmt::Display) -> String {
        e.to_string()
    }

    impl Tablet {
        pub fn connect() -> Result<Self, String> {
            let (conn, screen) = x11rb::connect(None).map_err(|e| format!("cannot reach the X server: {e}"))?;
            let root = conn.setup().roots[screen].root;
            if conn.extension_information(xinput::X11_EXTENSION_NAME).map_err(err)?.is_none() {
                return Err("the X server has no XInput".into());
            }
            let v = conn.xinput_xi_query_version(2, 2).map_err(err)?.reply().map_err(err)?;
            if v.major_version < 2 {
                return Err(format!("the X server has XInput {}.{}, 2.0 is needed", v.major_version, v.minor_version));
            }
            let label = conn.intern_atom(false, b"Abs Pressure").map_err(err)?.reply().map_err(err)?.atom;
            let mask = xinput::XIEventMask::RAW_MOTION
                | xinput::XIEventMask::RAW_BUTTON_PRESS
                | xinput::XIEventMask::RAW_BUTTON_RELEASE
                | xinput::XIEventMask::HIERARCHY;
            conn.xinput_xi_select_events(root, &[xinput::EventMask { deviceid: ALL_DEVICES, mask: vec![mask] }])
                .map_err(err)?
                .check()
                .map_err(err)?;
            let mut pressure = Pressure::default();
            pressure.set_pens(read_devices(&conn, label)?);
            let state = Arc::new(Mutex::new(pressure));
            let error = Arc::new(Mutex::new(None));
            let (s, e) = (state.clone(), error.clone());
            std::thread::Builder::new()
                .name("klay-tablet".into())
                .spawn(move || {
                    if let Err(msg) = listen(&conn, label, &s) {
                        *e.lock().unwrap_or_else(|p| p.into_inner()) = Some(msg);
                    }
                })
                .map_err(err)?;
            Ok(Self { state, error })
        }

        /// The thread's error, if it stopped.
        pub fn poll(&mut self) -> Result<(), String> {
            match self.error.lock().unwrap_or_else(|p| p.into_inner()).clone() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }

        pub fn state(&self) -> MutexGuard<'_, Pressure> {
            self.state.lock().unwrap_or_else(|p| p.into_inner())
        }
    }

    /// Move the pointer through XTest, from a connection of its own: the XTEST device has
    /// no pressure, so the listener should see it and report a mouse (`--selftest-pressure`).
    pub fn fake_motion(x: i16, y: i16) -> Result<(), String> {
        use x11rb::protocol::xtest::ConnectionExt as _;
        let (conn, screen) = x11rb::connect(None).map_err(err)?;
        let root = conn.setup().roots[screen].root;
        // MotionNotify, absolute.
        conn.xtest_fake_input(6, 0, 0, root, x, y, 0).map_err(err)?.check().map_err(err)?;
        Ok(())
    }

    /// Every slave pointer with a pressure axis.
    fn read_devices(conn: &RustConnection, label: u32) -> Result<Vec<Pen>, String> {
        let reply = conn.xinput_xi_query_device(ALL_DEVICES).map_err(err)?.reply().map_err(err)?;
        Ok(reply
            .infos
            .iter()
            .filter(|d| d.type_ == xinput::DeviceType::SLAVE_POINTER || d.type_ == xinput::DeviceType::FLOATING_SLAVE)
            .filter_map(|d| {
                let name = String::from_utf8_lossy(&d.name).into_owned();
                let valuators: Vec<(u16, u32, f64, f64)> = d
                    .classes
                    .iter()
                    .filter_map(|c| match &c.data {
                        xinput::DeviceClassData::Valuator(v) => Some((v.number, v.label, fp(v.min), fp(v.max))),
                        _ => None,
                    })
                    .collect();
                let (axis, min, max) = pressure_axis(&name, &valuators, label)?;
                Some(Pen::new(d.deviceid, &name, axis, min, max))
            })
            .collect())
    }

    /// The thread: wait for raw events and device changes until the connection fails.
    fn listen(conn: &RustConnection, label: u32, state: &Mutex<Pressure>) -> Result<(), String> {
        let lock = || state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            match conn.wait_for_event().map_err(err)? {
                Event::XinputRawMotion(e) | Event::XinputRawButtonPress(e) | Event::XinputRawButtonRelease(e) => {
                    let values: Vec<(i32, u32)> = e.axisvalues.iter().map(|v| (v.integral, v.frac)).collect();
                    let source = if e.sourceid != 0 { e.sourceid } else { e.deviceid };
                    lock().raw(source, &e.valuator_mask, &values);
                }
                Event::XinputHierarchy(_) => {
                    let pens = read_devices(conn, label)?;
                    lock().set_pens(pens);
                }
                _ => {}
            }
        }
    }
}

#[cfg(not(all(unix, not(target_vendor = "apple"), not(target_os = "android"))))]
mod x11 {
    use super::Pressure;
    use std::sync::{Mutex, MutexGuard};

    pub struct Tablet {
        state: Mutex<Pressure>,
    }

    impl Tablet {
        pub fn connect() -> Result<Self, String> {
            Err("pen pressure is only read on X11 so far".into())
        }

        pub fn poll(&mut self) -> Result<(), String> {
            Ok(())
        }

        pub fn state(&self) -> MutexGuard<'_, Pressure> {
            self.state.lock().unwrap_or_else(|p| p.into_inner())
        }
    }

    pub fn fake_motion(_x: i16, _y: i16) -> Result<(), String> {
        Err("no X11".into())
    }
}

pub use x11::{Tablet, fake_motion};

impl Tablet {
    /// The tablet for `window`: pressure is read on X11 only (not on Wayland yet).
    pub fn for_window(window: &winit::window::Window) -> Result<Self, String> {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_)) => Self::connect(),
            Ok(RawWindowHandle::Wayland(_)) => Err("pen pressure is read on X11 only; Wayland is not supported yet".into()),
            _ => Err("pen pressure is only read on X11 so far".into()),
        }
    }

    pub fn pressure(&self) -> Option<f32> {
        self.state().current()
    }

    /// One line for the interface: the pen in use, else the pens found.
    pub fn describe(&self) -> String {
        let state = self.state();
        if let Some(p) = state.active_pen() {
            return p.name.clone();
        }
        match state.pens.len() {
            0 => "none yet; touch the tablet with the pen".into(),
            1 => state.pens[0].name.clone(),
            n => format!("{n} pens: {}", state.pens.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 32.32 fixed point of `v`.
    fn fx(v: f64) -> (i32, u32) {
        let i = v.floor();
        (i as i32, ((v - i) * 4_294_967_296.0) as u32)
    }

    #[test]
    fn axis_values_follow_the_mask() {
        // Axes 0, 1, 2 and 4 carry values; 3 does not.
        let mask = [0b10111];
        let values = [fx(10.0), fx(20.0), fx(32767.5), fx(-3.0)];
        assert_eq!(axis_value(&mask, &values, 0), Some(10.0));
        assert_eq!(axis_value(&mask, &values, 2), Some(32767.5));
        assert_eq!(axis_value(&mask, &values, 3), None);
        assert_eq!(axis_value(&mask, &values, 4), Some(-3.0));
        assert_eq!(axis_value(&mask, &values, 40), None);
        // An axis past the first mask word.
        let mask = [0b1, 0b100];
        assert_eq!(axis_value(&mask, &[fx(1.0), fx(7.0)], 34), Some(7.0));
    }

    #[test]
    fn the_pressure_axis_is_found_by_label_then_by_name() {
        let label = 77;
        // libinput: x, y, pressure, tilt x, tilt y.
        let libinput = [(0, 10, 0.0, 34419.0), (1, 11, 0.0, 19461.0), (2, label, 0.0, 65535.0), (3, 12, -64.0, 63.0), (4, 13, -64.0, 63.0)];
        assert_eq!(pressure_axis("UGTABLET 15.6 inch PenDisplay Pen (0)", &libinput, label), Some((2, 0.0, 65535.0)));
        // A mouse: relative x, y and two scroll axes, none labelled pressure.
        let mouse = [(0, 20, -1.0, -1.0), (1, 21, -1.0, -1.0), (2, 22, 0.0, -1.0), (3, 23, 0.0, -1.0)];
        assert_eq!(pressure_axis("Logitech USB Optical Mouse", &mouse, label), None);
        // Unlabelled axes: only a device named like a pen counts.
        let bare = [(0, 0, 0.0, 1000.0), (1, 0, 0.0, 1000.0), (2, 0, 0.0, 8191.0)];
        assert_eq!(pressure_axis("XP-Pen stylus", &bare, label), Some((2, 0.0, 8191.0)));
        assert_eq!(pressure_axis("Touchpad", &bare, label), None);
        // The server has no "Abs Pressure" atom.
        assert_eq!(pressure_axis("Touchpad", &libinput, 0), None);
    }

    #[test]
    fn the_device_that_moved_last_decides() {
        let mut s = Pressure::default();
        s.set_pens(vec![Pen::new(12, "Pen (0)", 2, 0.0, 65535.0)]);
        assert_eq!(s.current(), None, "nothing moved yet");
        // The pen: x, y and pressure.
        s.raw(12, &[0b111], &[fx(100.0), fx(200.0), fx(32767.5)]);
        assert!((s.current().unwrap() - 0.5).abs() < 1e-4);
        // Only x changed: the pressure stays.
        s.raw(12, &[0b1], &[fx(101.0)]);
        assert!((s.current().unwrap() - 0.5).abs() < 1e-4);
        s.raw(12, &[0b100], &[fx(65535.0)]);
        assert_eq!(s.current(), Some(1.0));
        // Out of range values are clamped.
        s.raw(12, &[0b100], &[fx(70000.0)]);
        assert_eq!(s.current(), Some(1.0));
        // The mouse moves: full strength again.
        s.raw(9, &[0b11], &[fx(1.0), fx(1.0)]);
        assert_eq!(s.current(), None);
        assert!(s.active_pen().is_none());
        // Back to the pen, which kept its last pressure.
        s.raw(12, &[0b1], &[fx(5.0)]);
        assert_eq!(s.current(), Some(1.0));
        // Hot-plug: a second tool appears; the first keeps its pressure, a removed one is dropped.
        s.set_pens(vec![Pen::new(12, "Pen (0)", 2, 0.0, 65535.0), Pen::new(13, "Eraser (0)", 2, 0.0, 2047.0)]);
        assert_eq!(s.current(), Some(1.0));
        s.raw(13, &[0b100], &[fx(511.75)]);
        assert!((s.current().unwrap() - 0.25).abs() < 1e-4);
        s.set_pens(vec![Pen::new(12, "Pen (0)", 2, 0.0, 65535.0)]);
        assert_eq!(s.current(), None, "the eraser in use was unplugged");
    }
}
