//! Pen pressure from a drawing tablet (ZBrush: Brush > Tablet Pressure).
//!
//! winit reports no tablet pressure on Linux. On Wayland Klay binds the tablet protocol
//! (`zwp_tablet_manager_v2`) on winit's own connection; see the `wayland` module.
//!
//! On X11 Klay opens a second connection to
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
        let axis = self.pens.iter().find(|p| p.id == source).map(|p| p.axis);
        self.sample(source, axis.and_then(|a| axis_value(mask, values, a)));
    }

    /// Device `source` moved the pointer, with a new raw pressure `value` if it sent one.
    pub fn sample(&mut self, source: u16, value: Option<f64>) {
        self.events += 1;
        self.last_source = Some(source);
        let Some(pen) = self.pens.iter_mut().find(|p| p.id == source) else {
            self.active = None;
            return;
        };
        if let Some(v) = value {
            let span = pen.max - pen.min;
            pen.last = if span > 0.0 { ((v - pen.min) / span).clamp(0.0, 1.0) as f32 } else { 1.0 };
        }
        self.active = Some(pen.id);
    }

    /// The pen left the window (Wayland proximity out): no pressure until it is back.
    pub fn leave(&mut self) {
        self.active = None;
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
    pub struct Listener {
        state: Arc<Mutex<Pressure>>,
        error: Arc<Mutex<Option<String>>>,
    }

    fn fp(v: xinput::Fp3232) -> f64 {
        v.integral as f64 + v.frac as f64 / 4_294_967_296.0
    }

    fn err(e: impl std::fmt::Display) -> String {
        e.to_string()
    }

    impl Listener {
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

/// What a Wayland tablet tool did over Klay's window, in the order it happened. Once a
/// client binds the tablet protocol the compositor stops turning the pen into pointer
/// events for it, so these become the window's cursor and button events too.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PenEvent {
    Enter { pen: u16 },
    /// A new surface-local (logical) position and/or a new raw pressure, 0..=65535.
    Move { pen: u16, pos: Option<(f64, f64)>, pressure: Option<f64> },
    Button { pen: u16, button: winit::event::MouseButton, pressed: bool },
    Leave { pen: u16 },
    /// The pen tip went down or up on the title bar winit draws (a subsurface 35 px above
    /// the window), at `(x, y)` in its own coordinates; `serial` is the press's, which the
    /// compositor wants for a window move.
    Title { pen: u16, x: f64, y: f64, pressed: bool, serial: u32 },
    /// The pen's upper side button on the title bar: the window menu.
    TitleMenu { pen: u16, x: f64, y: f64, serial: u32 },
}

/// The window event a pen event stands for (`scale`: the window's scale factor), if any.
pub fn window_event(e: &PenEvent, scale: f64) -> Option<winit::event::WindowEvent> {
    use winit::event::{DeviceId, ElementState, WindowEvent};
    let device_id = DeviceId::dummy();
    Some(match *e {
        PenEvent::Enter { .. } => WindowEvent::CursorEntered { device_id },
        PenEvent::Move { pos: Some((x, y)), .. } => {
            WindowEvent::CursorMoved { device_id, position: winit::dpi::PhysicalPosition::new(x * scale, y * scale) }
        }
        PenEvent::Move { pos: None, .. } => return None,
        PenEvent::Button { button, pressed, .. } => WindowEvent::MouseInput {
            device_id,
            state: if pressed { ElementState::Pressed } else { ElementState::Released },
            button,
        },
        PenEvent::Leave { .. } => WindowEvent::CursorLeft { device_id },
        PenEvent::Title { .. } | PenEvent::TitleMenu { .. } => return None,
    })
}

/// What a pen tap on the title bar does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TitleAction {
    Close,
    ToggleMaximize,
    Minimize,
    Move,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TitleButton {
    Close,
    Maximize,
    Minimize,
}

/// winit's title bar on Wayland (sctk-adwaita 0.10), which only listens to the pointer:
/// its layout, so a pen tap can do what a click would. Sizes are sctk-adwaita's.
#[derive(Debug)]
pub struct TitleBar {
    left: Vec<TitleButton>,
    /// Right to left.
    right: Vec<TitleButton>,
    last_press: Option<std::time::Instant>,
}

impl TitleBar {
    const SIZE: f64 = 24.0;
    const MARGIN: f64 = 5.0;
    const SPACING: f64 = 13.0;
    /// sctk-adwaita's double-click time.
    const DOUBLE: std::time::Duration = std::time::Duration::from_millis(400);

    /// The layout from GNOME's `button-layout` ("appmenu:minimize,maximize,close"), as
    /// sctk-adwaita reads it; `None` gives its default (close, maximize, minimize on the right).
    pub fn new(layout: Option<&str>) -> Self {
        let side = |s: &str| -> Vec<TitleButton> {
            s.split(',')
                .take(3)
                .filter_map(|b| match b {
                    "close" => Some(TitleButton::Close),
                    "maximize" => Some(TitleButton::Maximize),
                    "minimize" => Some(TitleButton::Minimize),
                    _ => None,
                })
                .collect()
        };
        let parsed = layout.and_then(|l| l.split_once(':')).map(|(l, r)| (side(l), side(r).into_iter().rev().collect::<Vec<_>>()));
        let (left, right) = match parsed {
            Some((l, r)) if !l.is_empty() || !r.is_empty() => (l, r),
            _ => (Vec::new(), vec![TitleButton::Close, TitleButton::Maximize, TitleButton::Minimize]),
        };
        Self { left, right, last_press: None }
    }

    /// The layout GNOME asks for, through the desktop portal as sctk-adwaita does.
    pub fn from_desktop() -> Self {
        let out = std::process::Command::new("dbus-send")
            .args([
                "--reply-timeout=100",
                "--print-reply=literal",
                "--dest=org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.Settings.Read",
                "string:org.gnome.desktop.wm.preferences",
                "string:button-layout",
            ])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok());
        Self::new(out.as_deref().and_then(|s| s.rsplit(' ').next()))
    }

    /// The button at `(x, y)` on a title bar `width` wide (logical px).
    fn button(&self, x: f64, y: f64, width: f64, maximized: bool) -> Option<TitleButton> {
        if !(y > Self::MARGIN && y < Self::MARGIN + Self::SIZE) {
            return None;
        }
        let inside = |offset: f64| x > offset && x < offset + Self::SIZE;
        let mut left = Self::MARGIN + if maximized { 0.0 } else { 1.0 };
        for &b in &self.left {
            if inside(left) {
                return Some(b);
            }
            left += Self::SIZE + Self::SPACING;
        }
        let mut right = width - Self::MARGIN;
        for &b in &self.right {
            right -= Self::SIZE;
            if inside(right) {
                return Some(b);
            }
            right -= Self::SPACING;
        }
        None
    }

    /// A pen press or release at `(x, y)`: buttons act on release, the rest of the bar
    /// moves the window on press, or toggles maximize on a double tap.
    pub fn tap(&mut self, x: f64, y: f64, pressed: bool, width: f64, maximized: bool, now: std::time::Instant) -> Option<TitleAction> {
        match self.button(x, y, width, maximized) {
            Some(b) if !pressed => Some(match b {
                TitleButton::Close => TitleAction::Close,
                TitleButton::Maximize => TitleAction::ToggleMaximize,
                TitleButton::Minimize => TitleAction::Minimize,
            }),
            Some(_) => None,
            None if pressed => match self.last_press.replace(now) {
                Some(last) if now.saturating_duration_since(last) < Self::DOUBLE => {
                    self.last_press = None;
                    Some(TitleAction::ToggleMaximize)
                }
                _ => Some(TitleAction::Move),
            },
            None => None,
        }
    }
}

/// The button a tool's evdev button code stands for: the tip is Left; the stylus buttons
/// are Middle and Right, as GTK and mutter's pointer emulation map them.
fn tool_button(code: u32) -> Option<winit::event::MouseButton> {
    use winit::event::MouseButton;
    Some(match code {
        0x110 => MouseButton::Left,   // BTN_LEFT (a tablet mouse)
        0x111 => MouseButton::Right,  // BTN_RIGHT
        0x112 => MouseButton::Middle, // BTN_MIDDLE
        0x14b => MouseButton::Middle, // BTN_STYLUS
        0x14c => MouseButton::Right,  // BTN_STYLUS2
        0x149 => MouseButton::Back,   // BTN_STYLUS3
        _ => return None,
    })
}

/// One tool's events between two `frame`s, kept apart from the connection so it can be
/// tested.
#[derive(Default, Debug)]
struct Frame {
    enter: bool,
    leave: bool,
    pos: Option<(f64, f64)>,
    pressure: Option<f64>,
    buttons: Vec<(winit::event::MouseButton, bool)>,
}

impl Frame {
    /// The frame as events: enter, then where the pen is, then its buttons (so a press
    /// lands at the new position), then leave. `held` (buttons down) is kept up to date,
    /// and anything still held when the pen leaves is released first.
    fn take(&mut self, pen: u16, held: &mut Vec<winit::event::MouseButton>) -> Vec<PenEvent> {
        let f = std::mem::take(self);
        let mut out = Vec::new();
        if f.enter {
            out.push(PenEvent::Enter { pen });
        }
        if f.pos.is_some() || f.pressure.is_some() {
            out.push(PenEvent::Move { pen, pos: f.pos, pressure: f.pressure });
        }
        for (button, pressed) in f.buttons {
            if pressed == held.contains(&button) {
                continue;
            }
            if pressed {
                held.push(button);
            } else {
                held.retain(|b| *b != button);
            }
            out.push(PenEvent::Button { pen, button, pressed });
        }
        if f.leave {
            for button in held.drain(..) {
                out.push(PenEvent::Button { pen, button, pressed: false });
            }
            out.push(PenEvent::Leave { pen });
        }
        out
    }
}

#[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
mod wayland {
    //! The tablet protocol on winit's own `wl_display`: the compositor sends tablet events
    //! only to the client whose surface the pen is over, so a connection of our own would
    //! hear nothing. The objects live on an event queue of their own, dispatched by a
    //! thread; libwayland lets that thread and winit both read the socket.
    use super::{Frame, Pen, PenEvent, Pressure, tool_button};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};
    use wayland_backend::client::{Backend, ObjectId};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{wl_callback, wl_registry, wl_seat};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, event_created_child};
    use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;
    use wayland_protocols::wp::tablet::zv2::client::{
        zwp_tablet_manager_v2::ZwpTabletManagerV2, zwp_tablet_pad_dial_v2::ZwpTabletPadDialV2,
        zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2}, zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2,
        zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2, zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
        zwp_tablet_seat_v2::{self, ZwpTabletSeatV2}, zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
        zwp_tablet_v2::{self, ZwpTabletV2},
    };

    /// What the thread hands the main loop.
    #[derive(Default)]
    struct Shared {
        events: VecDeque<PenEvent>,
        /// The tools, when they changed since the main loop last looked.
        pens: Option<Vec<Pen>>,
        error: Option<String>,
    }

    struct Tool {
        proxy: ZwpTabletToolV2,
        num: u16,
        kind: &'static str,
        tablet: String,
        /// Over Klay's window (not a decoration or another client).
        over: bool,
        frame: Frame,
        held: Vec<winit::event::MouseButton>,
        /// A decoration surface the tool is over instead, and where on it.
        deco: Option<ObjectId>,
        deco_pos: (f64, f64),
        /// Tip presses and releases there (with the press's serial), and an upper side button
        /// press, sent on `frame` if it is the title bar.
        deco_taps: Vec<(bool, u32)>,
        deco_menu: Option<u32>,
    }

    /// Whether a point on one of winit's decoration surfaces can only be on the title bar.
    /// sctk-adwaita's title bar is 35 px tall; the borders take input only in a 12 px strip
    /// along the window (left: x 32..44, right: x 0..12, top: y 32..44, bottom: y 0..12).
    fn only_title_bar(x: f64, y: f64) -> bool {
        (12.0..32.0).contains(&y) && (x >= 44.0 || (12.0..32.0).contains(&x))
    }

    struct State {
        shared: Arc<Mutex<Shared>>,
        /// Klay's `wl_surface`.
        surface: usize,
        next: u16,
        tablets: Vec<(ObjectId, String)>,
        tools: Vec<Tool>,
        /// winit's title bar surface, once a pen has been somewhere only it covers.
        title_bar: Option<ObjectId>,
    }

    impl State {
        fn shared(&self) -> MutexGuard<'_, Shared> {
            self.shared.lock().unwrap_or_else(|p| p.into_inner())
        }

        fn publish_pens(&self) {
            let pens = self
                .tools
                .iter()
                .map(|t| {
                    let name = if t.tablet.is_empty() { t.kind.to_string() } else { format!("{} {}", t.tablet, t.kind) };
                    Pen::new(t.num, &name, 0, 0.0, 65535.0)
                })
                .collect();
            self.shared().pens = Some(pens);
        }
    }

    pub struct Listener {
        shared: Arc<Mutex<Shared>>,
        /// Read by the main loop only, as it replays the events.
        state: Mutex<Pressure>,
        /// For window moves and the window menu, which the compositor grants only with the
        /// pen's own serial on the seat it came from.
        conn: Connection,
        seat: wl_seat::WlSeat,
        /// Stopping the thread: the flag, and the queue a `wl_display.sync` wakes it on.
        qh: QueueHandle<State>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    /// The thread blocks on winit's display, so it must be gone before winit disconnects
    /// it (`App::exiting`), or it reads freed memory as Klay quits.
    impl Drop for Listener {
        fn drop(&mut self) {
            let Some(thread) = self.thread.take() else { return };
            self.stop.store(true, Ordering::Release);
            // The compositor's answer wakes the thread, which then sees the flag.
            // (A dead connection has already ended the thread with an error.)
            self.conn.display().sync(&self.qh, ());
            let _ = self.conn.flush();
            let _ = thread.join();
        }
    }

    impl Dispatch<wl_callback::WlCallback, ()> for State {
        fn event(_: &mut Self, _: &wl_callback::WlCallback, _: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    fn err(e: impl std::fmt::Display) -> String {
        e.to_string()
    }

    impl Listener {
        /// Listen on `display` (winit's `wl_display`) for tools over `surface`.
        ///
        /// # Safety
        /// `display` must stay connected for as long as the process runs.
        pub unsafe fn connect(display: *mut std::ffi::c_void, surface: *mut std::ffi::c_void) -> Result<Self, String> {
            let backend = unsafe { Backend::from_foreign_display(display.cast()) };
            let conn = Connection::from_backend(backend);
            let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(err)?;
            let qh = queue.handle();
            let manager: ZwpTabletManagerV2 = globals
                .bind(&qh, 1..=1, ())
                .map_err(|_| "the compositor does not offer the tablet protocol (zwp_tablet_manager_v2)".to_string())?;
            let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).map_err(|_| "the compositor has no seat".to_string())?;
            manager.get_tablet_seat(&seat, &qh, ());
            let shared = Arc::new(Mutex::new(Shared::default()));
            let (conn2, seat2) = (conn.clone(), seat.clone());
            let mut state = State { shared: shared.clone(), surface: surface as usize, next: 1, tablets: Vec::new(), tools: Vec::new(), title_bar: None };
            // The tablets and tools already plugged in.
            queue.roundtrip(&mut state).map_err(err)?;
            state.publish_pens();
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = stop.clone();
            let thread = std::thread::Builder::new()
                .name("klay-tablet".into())
                .spawn(move || {
                    while !stopping.load(Ordering::Acquire) {
                        if let Err(e) = queue.blocking_dispatch(&mut state) {
                            state.shared().error = Some(e.to_string());
                            break;
                        }
                    }
                    // Everything on the queue goes before winit closes the display.
                    drop((state, manager, seat, queue));
                })
                .map_err(err)?;
            let listener = Self {
                shared,
                state: Mutex::new(Pressure::default()),
                conn: conn2,
                seat: seat2,
                qh,
                stop,
                thread: Some(thread),
            };
            drop(listener.state());
            Ok(listener)
        }

        pub fn poll(&mut self) -> Result<(), String> {
            match self.shared.lock().unwrap_or_else(|p| p.into_inner()).error.clone() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }

        pub fn state(&self) -> MutexGuard<'_, Pressure> {
            let pens = self.shared.lock().unwrap_or_else(|p| p.into_inner()).pens.take();
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(pens) = pens {
                state.set_pens(pens);
            }
            state
        }

        /// winit's `xdg_toplevel` (`toplevel`) as a proxy on this connection.
        fn toplevel(&self, toplevel: *mut std::ffi::c_void) -> Result<XdgToplevel, String> {
            // Safety: winit's toplevel lives as long as its window, which outlives this call.
            let id = unsafe { ObjectId::from_ptr(XdgToplevel::interface(), toplevel.cast()) }.map_err(err)?;
            XdgToplevel::from_id(&self.conn, id).map_err(err)
        }

        /// Move the window with the pen, as a title bar drag does with the pointer.
        pub fn move_window(&self, toplevel: *mut std::ffi::c_void, serial: u32) -> Result<(), String> {
            self.toplevel(toplevel)?._move(&self.seat, serial);
            self.conn.flush().map_err(err)
        }

        /// The compositor's window menu at `(x, y)` (window geometry coordinates).
        pub fn window_menu(&self, toplevel: *mut std::ffi::c_void, serial: u32, x: i32, y: i32) -> Result<(), String> {
            self.toplevel(toplevel)?.show_window_menu(&self.seat, serial, x, y);
            self.conn.flush().map_err(err)
        }

        pub fn take_events(&self) -> Vec<PenEvent> {
            self.shared.lock().unwrap_or_else(|p| p.into_inner()).events.drain(..).collect()
        }
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
        fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<wl_seat::WlSeat, ()> for State {
        fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<ZwpTabletManagerV2, ()> for State {
        fn event(_: &mut Self, _: &ZwpTabletManagerV2, _: <ZwpTabletManagerV2 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<ZwpTabletSeatV2, ()> for State {
        fn event(state: &mut Self, _: &ZwpTabletSeatV2, event: zwp_tablet_seat_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            match event {
                zwp_tablet_seat_v2::Event::TabletAdded { id } => state.tablets.push((id.id(), String::new())),
                zwp_tablet_seat_v2::Event::ToolAdded { id } => {
                    let num = state.next;
                    state.next = state.next.wrapping_add(1).max(1);
                    state.tools.push(Tool {
                        proxy: id,
                        num,
                        kind: "Pen",
                        tablet: String::new(),
                        over: false,
                        frame: Frame::default(),
                        held: Vec::new(),
                        deco: None,
                        deco_pos: (0.0, 0.0),
                        deco_taps: Vec::new(),
                        deco_menu: None,
                    });
                }
                _ => {}
            }
        }

        event_created_child!(State, ZwpTabletSeatV2, [
            zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
            zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ()),
            zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, ()),
        ]);
    }

    impl Dispatch<ZwpTabletV2, ()> for State {
        fn event(state: &mut Self, tablet: &ZwpTabletV2, event: zwp_tablet_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            match event {
                zwp_tablet_v2::Event::Name { name } => {
                    if let Some(t) = state.tablets.iter_mut().find(|t| t.0 == tablet.id()) {
                        t.1 = name;
                    }
                }
                zwp_tablet_v2::Event::Removed => {
                    state.tablets.retain(|t| t.0 != tablet.id());
                    tablet.destroy();
                }
                _ => {}
            }
        }
    }

    impl Dispatch<ZwpTabletToolV2, ()> for State {
        fn event(state: &mut Self, tool: &ZwpTabletToolV2, event: zwp_tablet_tool_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            use zwp_tablet_tool_v2::{ButtonState, Event, Type};
            let Some(i) = state.tools.iter().position(|t| t.proxy == *tool) else { return };
            let surface = state.surface;
            let t = &mut state.tools[i];
            match event {
                Event::Type { tool_type } => {
                    t.kind = match tool_type {
                        WEnum::Value(Type::Eraser) => "Eraser",
                        WEnum::Value(Type::Brush) => "Brush",
                        WEnum::Value(Type::Pencil) => "Pencil",
                        WEnum::Value(Type::Airbrush) => "Airbrush",
                        WEnum::Value(Type::Finger) => "Finger",
                        WEnum::Value(Type::Mouse) => "Mouse",
                        WEnum::Value(Type::Lens) => "Lens",
                        _ => "Pen",
                    }
                }
                Event::Done => state.publish_pens(),
                Event::Removed => {
                    tool.destroy();
                    state.tools.remove(i);
                    state.publish_pens();
                }
                Event::ProximityIn { tablet, surface: s, .. } => {
                    if s.id().as_ptr() as usize == surface {
                        t.over = true;
                        t.frame.enter = true;
                        t.frame.leave = false;
                    } else {
                        t.deco = Some(s.id());
                    }
                    let name = state.tablets.iter().find(|x| x.0 == tablet.id()).map(|x| x.1.clone()).unwrap_or_default();
                    if state.tools[i].tablet != name {
                        state.tools[i].tablet = name;
                        state.publish_pens();
                    }
                }
                Event::ProximityOut => {
                    t.deco = None;
                    t.deco_taps.clear();
                    t.deco_menu = None;
                    if t.over {
                        t.over = false;
                        t.frame.leave = true;
                    }
                }
                Event::Down { .. } if t.over => t.frame.buttons.push((winit::event::MouseButton::Left, true)),
                Event::Down { serial } if t.deco.is_some() => t.deco_taps.push((true, serial)),
                Event::Up => {
                    t.frame.buttons.push((winit::event::MouseButton::Left, false));
                    if t.deco.is_some() {
                        t.deco_taps.push((false, 0));
                    }
                }
                Event::Button { serial, button: 0x14c, state: WEnum::Value(ButtonState::Pressed) } if t.deco.is_some() => {
                    t.deco_menu = Some(serial);
                }
                Event::Motion { x, y } if t.over => t.frame.pos = Some((x, y)),
                Event::Motion { x, y } => {
                    if let Some(id) = &t.deco {
                        t.deco_pos = (x, y);
                        if state.title_bar.as_ref() != Some(id) && only_title_bar(x, y) {
                            state.title_bar = Some(id.clone());
                        }
                    }
                }
                Event::Pressure { pressure } => t.frame.pressure = Some(pressure as f64),
                Event::Button { button, state: b, .. } => {
                    if let Some(button) = tool_button(button) {
                        let pressed = b == WEnum::Value(ButtonState::Pressed);
                        if t.over || !pressed {
                            t.frame.buttons.push((button, pressed));
                        }
                    }
                }
                Event::Frame { .. } => {
                    let mut events = t.frame.take(t.num, &mut t.held);
                    let on_title = t.deco.is_some() && t.deco == state.title_bar;
                    let (x, y) = t.deco_pos;
                    for (pressed, serial) in t.deco_taps.drain(..) {
                        if on_title {
                            events.push(PenEvent::Title { pen: t.num, x, y, pressed, serial });
                        }
                    }
                    if let Some(serial) = t.deco_menu.take().filter(|_| on_title) {
                        events.push(PenEvent::TitleMenu { pen: t.num, x, y, serial });
                    }
                    if !events.is_empty() {
                        state.shared().events.extend(events);
                    }
                }
                _ => {}
            }
        }
    }

    // Pads (the tablet's own buttons, rings and strips) are not used, but their objects
    // must exist for the protocol to go on.
    impl Dispatch<ZwpTabletPadV2, ()> for State {
        fn event(_: &mut Self, pad: &ZwpTabletPadV2, event: zwp_tablet_pad_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            if let zwp_tablet_pad_v2::Event::Removed = event {
                pad.destroy();
            }
        }

        event_created_child!(State, ZwpTabletPadV2, [
            zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, ()),
        ]);
    }

    impl Dispatch<ZwpTabletPadGroupV2, ()> for State {
        fn event(_: &mut Self, _: &ZwpTabletPadGroupV2, _: zwp_tablet_pad_group_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}

        event_created_child!(State, ZwpTabletPadGroupV2, [
            zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (ZwpTabletPadRingV2, ()),
            zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, ()),
            zwp_tablet_pad_group_v2::EVT_DIAL_OPCODE => (ZwpTabletPadDialV2, ()),
        ]);
    }

    impl Dispatch<ZwpTabletPadRingV2, ()> for State {
        fn event(_: &mut Self, _: &ZwpTabletPadRingV2, _: <ZwpTabletPadRingV2 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<ZwpTabletPadStripV2, ()> for State {
        fn event(_: &mut Self, _: &ZwpTabletPadStripV2, _: <ZwpTabletPadStripV2 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<ZwpTabletPadDialV2, ()> for State {
        fn event(_: &mut Self, _: &ZwpTabletPadDialV2, _: <ZwpTabletPadDialV2 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }
}

#[cfg(not(all(unix, not(target_vendor = "apple"), not(target_os = "android"))))]
mod x11 {
    use super::Pressure;
    use std::sync::{Mutex, MutexGuard};

    pub struct Listener {
        state: Mutex<Pressure>,
    }

    impl Listener {
        pub fn connect() -> Result<Self, String> {
            Err("pen pressure is only read on Linux so far".into())
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

pub use x11::fake_motion;

enum Backend {
    X11(x11::Listener),
    #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
    Wayland(wayland::Listener),
}

/// The pen-pressure listener for Klay's window.
pub struct Tablet {
    backend: Backend,
    /// Wayland: winit's title bar, which the pen cannot click by itself.
    title: Option<TitleBar>,
}

impl Tablet {
    /// The tablet for `window`: XInput2 on X11, the tablet protocol on Wayland.
    pub fn for_window(window: &winit::window::Window) -> Result<Self, String> {
        use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
        let mut title = None;
        let backend = match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_)) => Backend::X11(x11::Listener::connect()?),
            #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
            Ok(RawWindowHandle::Wayland(w)) => {
                let Ok(RawDisplayHandle::Wayland(d)) = window.display_handle().map(|h| h.as_raw()) else {
                    return Err("no Wayland display handle".into());
                };
                title = Some(TitleBar::from_desktop());
                // Safety: winit keeps its display connected until the process exits.
                Backend::Wayland(unsafe { wayland::Listener::connect(d.display.as_ptr(), w.surface.as_ptr())? })
            }
            _ => return Err("pen pressure is only read on X11 and Wayland so far".into()),
        };
        Ok(Self { backend, title })
    }

    /// Wayland: start moving `window` with the pen whose press had `serial`.
    pub fn move_window(&self, window: &winit::window::Window, serial: u32) -> Result<(), String> {
        match &self.backend {
            #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
            Backend::Wayland(l) => {
                use winit::platform::wayland::WindowExtWayland;
                let top = window.xdg_toplevel().ok_or("the window has no xdg_toplevel")?;
                l.move_window(top.as_ptr(), serial)
            }
            _ => window.drag_window().map_err(|e| e.to_string()),
        }
    }

    /// Wayland: the compositor's window menu for a pen on the title bar at `(x, y)`.
    pub fn window_menu(&self, window: &winit::window::Window, serial: u32, x: f64, y: f64) -> Result<(), String> {
        match &self.backend {
            #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
            Backend::Wayland(l) => {
                use winit::platform::wayland::WindowExtWayland;
                let top = window.xdg_toplevel().ok_or("the window has no xdg_toplevel")?;
                // The title bar sits 35 px above the window's content.
                l.window_menu(top.as_ptr(), serial, x as i32, y as i32 - 35)
            }
            _ => Ok(()),
        }
    }

    /// What a pen tap on the title bar does (`width`: the window's logical width).
    pub fn title_action(&mut self, e: &PenEvent, width: f64, maximized: bool) -> Option<TitleAction> {
        let PenEvent::Title { x, y, pressed, .. } = *e else { return None };
        self.title.as_mut()?.tap(x, y, pressed, width, maximized, std::time::Instant::now())
    }

    /// "XInput2" or "Wayland tablet protocol", for logs.
    pub fn kind(&self) -> &'static str {
        match self.backend {
            Backend::X11(_) => "XInput2",
            #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
            Backend::Wayland(_) => "Wayland tablet protocol",
        }
    }

    /// The listener's error, if it stopped.
    pub fn poll(&mut self) -> Result<(), String> {
        match &mut self.backend {
            Backend::X11(l) => l.poll(),
            #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
            Backend::Wayland(l) => l.poll(),
        }
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, Pressure> {
        match &self.backend {
            Backend::X11(l) => l.state(),
            #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
            Backend::Wayland(l) => l.state(),
        }
    }

    /// Pen events (Wayland only) the main loop has not replayed yet.
    pub fn take_events(&self) -> Vec<PenEvent> {
        match &self.backend {
            #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
            Backend::Wayland(l) => l.take_events(),
            _ => Vec::new(),
        }
    }

    /// Bring the pressure up to `e`, just before it is replayed as a window event, so each
    /// move is drawn at its own pressure.
    pub fn apply(&self, e: &PenEvent) {
        let mut s = self.state();
        match *e {
            PenEvent::Enter { pen } => s.sample(pen, None),
            PenEvent::Move { pen, pressure, .. } => s.sample(pen, pressure),
            PenEvent::Button { .. } | PenEvent::Title { .. } | PenEvent::TitleMenu { .. } => {}
            PenEvent::Leave { .. } => s.leave(),
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

    #[test]
    fn a_wayland_frame_becomes_events_in_order() {
        use winit::event::MouseButton::{Left, Middle};
        let mut held = Vec::new();
        // The pen comes over the window, touches down and moves in one frame.
        let mut f = Frame { enter: true, pos: Some((10.0, 20.0)), pressure: Some(100.0), buttons: vec![(Left, true)], ..Frame::default() };
        assert_eq!(
            f.take(3, &mut held),
            vec![
                PenEvent::Enter { pen: 3 },
                PenEvent::Move { pen: 3, pos: Some((10.0, 20.0)), pressure: Some(100.0) },
                PenEvent::Button { pen: 3, button: Left, pressed: true },
            ]
        );
        assert_eq!(held, vec![Left]);
        assert!(f.take(3, &mut held).is_empty(), "the frame was emptied");
        // Pressure alone, a repeated press and a side button.
        let mut f = Frame { pressure: Some(5.0), buttons: vec![(Left, true), (Middle, true)], ..Frame::default() };
        assert_eq!(
            f.take(3, &mut held),
            vec![PenEvent::Move { pen: 3, pos: None, pressure: Some(5.0) }, PenEvent::Button { pen: 3, button: Middle, pressed: true }]
        );
        // The pen leaves with buttons still down: they are released first.
        let mut f = Frame { leave: true, ..Frame::default() };
        let out = f.take(3, &mut held);
        assert_eq!(out.len(), 3);
        assert!(out[..2].iter().all(|e| matches!(e, PenEvent::Button { pressed: false, .. })));
        assert_eq!(out[2], PenEvent::Leave { pen: 3 });
        assert!(held.is_empty());
        // A release of a button that is not down is dropped.
        let mut f = Frame { buttons: vec![(Left, false)], ..Frame::default() };
        assert!(f.take(3, &mut held).is_empty());
    }

    #[test]
    fn pen_events_replay_as_window_events_with_pressure() {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        let moved = window_event(&PenEvent::Move { pen: 1, pos: Some((100.0, 50.5)), pressure: None }, 1.5);
        match moved {
            Some(WindowEvent::CursorMoved { position, .. }) => assert_eq!((position.x, position.y), (150.0, 75.75)),
            e => panic!("{e:?}"),
        }
        assert!(window_event(&PenEvent::Move { pen: 1, pos: None, pressure: Some(1.0) }, 1.0).is_none());
        assert!(matches!(
            window_event(&PenEvent::Button { pen: 1, button: MouseButton::Left, pressed: false }, 1.0),
            Some(WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. })
        ));
        assert_eq!(tool_button(0x14b), Some(MouseButton::Middle));
        assert_eq!(tool_button(0x14c), Some(MouseButton::Right));
        assert_eq!(tool_button(0x1), None);
        // The pressure a Wayland tool reports (0..=65535), and leaving the window.
        let mut s = Pressure::default();
        s.set_pens(vec![Pen::new(1, "Pen", 0, 0.0, 65535.0)]);
        s.sample(1, None);
        assert_eq!(s.current(), Some(0.0));
        s.sample(1, Some(65535.0 / 4.0));
        assert!((s.current().unwrap() - 0.25).abs() < 1e-4);
        s.leave();
        assert_eq!(s.current(), None);
    }

    #[test]
    fn pen_taps_on_the_title_bar() {
        use std::time::{Duration, Instant};
        // GNOME's default: close rightmost, then maximize, then minimize.
        let mut bar = TitleBar::new(Some("appmenu:minimize,maximize,close"));
        let (w, t0) = (1600.0, Instant::now());
        let y = 17.0;
        // Close spans x 1571..1595, maximize 1534..1558, minimize 1497..1521; buttons act on release.
        assert_eq!(bar.tap(1583.0, y, true, w, false, t0), None);
        assert_eq!(bar.tap(1583.0, y, false, w, false, t0), Some(TitleAction::Close));
        assert_eq!(bar.tap(1546.0, y, false, w, false, t0), Some(TitleAction::ToggleMaximize));
        assert_eq!(bar.tap(1509.0, y, false, w, false, t0), Some(TitleAction::Minimize));
        // Between buttons, or above them, is the bar itself: a press moves, a double tap maximizes.
        assert_eq!(bar.tap(1527.0, y, true, w, false, t0), Some(TitleAction::Move));
        assert_eq!(bar.tap(800.0, 3.0, true, w, false, t0 + Duration::from_millis(250)), Some(TitleAction::ToggleMaximize));
        assert_eq!(bar.tap(800.0, y, true, w, false, t0 + Duration::from_millis(400)), Some(TitleAction::Move));
        assert_eq!(bar.tap(800.0, y, true, w, false, t0 + Duration::from_millis(900)), Some(TitleAction::Move));
        assert_eq!(bar.tap(800.0, y, false, w, false, t0), None);
        // Buttons on the left, and sctk-adwaita's default when there is no setting.
        let mut left = TitleBar::new(Some("close,minimize:"));
        assert_eq!(left.tap(18.0, y, false, w, false, t0), Some(TitleAction::Close));
        assert_eq!(left.tap(55.0, y, false, w, true, t0), Some(TitleAction::Minimize));
        let mut default = TitleBar::new(None);
        assert_eq!(default.tap(1583.0, y, false, w, false, t0), Some(TitleAction::Close));
        assert_eq!(default.tap(1509.0, y, false, w, false, t0), Some(TitleAction::Minimize));
    }
}
