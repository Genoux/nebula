//! The floating pill: a wlr-layer-shell surface painted by hand.
//!
//! Mapped only while there is something to show and destroyed when it fades
//! out, so between uses Nebula composites nothing and takes no input. The
//! surface never takes keyboard focus; clicks are the only input it accepts -
//! left toggles mute, right ends the session.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::os::unix::fs::FileExt;
use std::sync::mpsc::Receiver;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use wayland_client::backend::WaylandError;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use crate::link::Link;
use crate::look::{self, Motion, Rgb};
use crate::paint::{self, Canvas, Rect, Text};
use nebula_core::config::Ui;
use nebula_core::event::{UiCommand, UiStatus};

const WIDTH: u32 = 240;
const HEIGHT: u32 = 44;
const MARGIN: i32 = 12;
const FONT_PX: f32 = 13.0;
/// ~30fps while visible: the motion is slow pulses and smoothed bars, and a
/// status indicator has no business competing with the desktop for frames.
const FRAME: Duration = Duration::from_millis(33);
/// How often a hidden HUD wakes to check for a new status.
const IDLE_WAKE: Duration = Duration::from_millis(250);
const FADE_IN: f32 = 0.15;
const FADE_OUT: f32 = 0.30;
/// Linux input event codes, as `wl_pointer.button` reports them.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

#[derive(Default)]
struct Wayland {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    scale: i32,
    configured: bool,
    closed: bool,
    clicks: Vec<u32>,
}

pub struct Hud {
    _connection: Connection,
    queue: EventQueue<Wayland>,
    state: Wayland,
}

impl Hud {
    /// Fails without a Wayland display or without layer-shell, which is the
    /// caller's signal to fall back to notifications.
    pub fn connect() -> Result<Self> {
        let connection = Connection::connect_to_env().context("no Wayland display")?;
        let mut queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let mut state = Wayland {
            scale: 1,
            ..Default::default()
        };
        queue
            .roundtrip(&mut state)
            .context("wayland registry roundtrip")?;
        if state.layer_shell.is_none() {
            bail!("the compositor does not offer wlr-layer-shell");
        }
        if state.compositor.is_none() || state.shm.is_none() {
            bail!("the compositor does not offer wl_compositor and wl_shm");
        }
        Ok(Self {
            _connection: connection,
            queue,
            state,
        })
    }

    pub fn run(mut self, ui: &Ui, statuses: &Receiver<UiStatus>, link: &Link) -> Result<()> {
        let hide_after = Duration::from_secs(ui.hide_after_seconds);
        let at_top = anchored_top(&ui.position);
        let mut text = Text::load(FONT_PX);
        if text.is_none() {
            tracing::warn!("no font found - the HUD will draw without labels");
        }

        let started = Instant::now();
        let mut status = UiStatus::Standby;
        let mut changed_at = started;
        let mut last_frame = started;
        let mut opacity = 0.0f32;
        let mut shown_level = 0.0f32;
        let mut island: Option<Island> = None;
        let mut buffers: Option<Buffers> = None;

        loop {
            let now = Instant::now();
            // Capped so the first frame after an idle wake fades rather than
            // jumping straight to fully drawn.
            let dt = (now - last_frame).as_secs_f32().min(0.1);
            last_frame = now;

            while let Ok(next) = statuses.try_recv() {
                if !look::same_kind(&status, &next) {
                    changed_at = now;
                }
                status = next;
            }
            for button in self.state.clicks.drain(..) {
                match button {
                    BTN_LEFT => link.send(UiCommand::ToggleMute),
                    BTN_RIGHT => link.send(UiCommand::EndSession),
                    _ => {}
                }
            }
            if self.state.closed {
                island = None;
                buffers = None;
                self.state.closed = false;
                self.state.configured = false;
            }

            let wanted = look::stays_visible(&status, now - changed_at, hide_after);
            opacity = step_opacity(opacity, wanted, dt);
            shown_level += (look::level(&status) - shown_level) * 0.35;

            let qh = self.queue.handle();
            if (wanted || opacity > 0.0) && island.is_none() {
                island = Some(Island::map(&self.state, &qh, at_top));
            }
            if !wanted && opacity <= 0.0 && island.is_some() {
                island = None;
                buffers = None;
                self.state.configured = false;
            }

            if let Some(mapped) = island.as_ref().filter(|_| self.state.configured) {
                let scale = self.state.scale.max(1);
                if buffers.as_ref().is_none_or(|b| b.scale != scale) {
                    let shm = self.state.shm.as_ref().expect("checked in connect");
                    buffers = Some(Buffers::create(shm, &qh, scale)?);
                }
                let frame = buffers.as_mut().expect("created above");
                let elapsed = (now - started).as_secs_f32();
                draw(
                    &mut frame.canvas,
                    &status,
                    text.as_mut(),
                    shown_level,
                    elapsed,
                    opacity,
                    scale as f32,
                );
                frame.present(&mapped.surface)?;
            }

            self.pump(if island.is_some() { FRAME } else { IDLE_WAKE })?;
        }
    }

    /// Dispatch whatever the compositor has sent, waiting at most `timeout` for
    /// more. Polling the socket rather than blocking on it is what lets the
    /// same thread also notice new statuses from the daemon.
    fn pump(&mut self, timeout: Duration) -> Result<()> {
        self.queue.dispatch_pending(&mut self.state)?;
        self.queue.flush().context("flushing to the compositor")?;
        if let Some(guard) = self.queue.prepare_read() {
            let mut fds = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd that outlives the call.
            let ready = unsafe { libc::poll(&mut fds, 1, timeout.as_millis() as i32) };
            if ready > 0 {
                match guard.read() {
                    Ok(_) => {}
                    Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e).context("reading from the compositor"),
                }
            }
        }
        self.queue.dispatch_pending(&mut self.state)?;
        Ok(())
    }
}

fn step_opacity(current: f32, wanted: bool, dt: f32) -> f32 {
    if wanted {
        (current + dt / FADE_IN).min(1.0)
    } else {
        (current - dt / FADE_OUT).max(0.0)
    }
}

fn anchored_top(position: &str) -> bool {
    position.trim().starts_with("top")
}

fn draw(
    canvas: &mut Canvas,
    status: &UiStatus,
    text: Option<&mut Text>,
    level: f32,
    seconds: f32,
    opacity: f32,
    s: f32,
) {
    canvas.clear();
    let look = look::look(status);
    let (w, h) = (WIDTH as f32 * s, HEIGHT as f32 * s);

    let pill = Rect {
        x: 0.5 * s,
        y: 0.5 * s,
        w: w - s,
        h: h - s,
    };
    canvas.rounded_rect(pill, pill.h / 2.0, look::PILL, 0.78 * opacity);
    canvas.ring(pill, pill.h / 2.0, s, look::INK, 0.10 * opacity);

    let (orb_x, orb_y) = (22.0 * s, h / 2.0);
    let (radius, glow) = match look.motion {
        Motion::Pulse { hz } => {
            let p = look::pulse(hz, seconds);
            (7.0 + 2.0 * p, 0.18 + 0.22 * p)
        }
        Motion::Bars => (7.0 + 2.0 * level, 0.18 + 0.3 * level),
        Motion::Still => (7.0, 0.12),
    };
    canvas.circle(
        orb_x,
        orb_y,
        (radius + 5.0) * s,
        look.color,
        glow * look.orb_alpha * opacity,
    );
    canvas.circle(
        orb_x,
        orb_y,
        radius * s,
        look.color,
        look.orb_alpha * opacity,
    );

    let bars = if look.motion == Motion::Bars {
        draw_bars(canvas, level, seconds, look.color, opacity, s)
    } else {
        0.0
    };

    if let Some(text) = text {
        text.set_px(FONT_PX * s);
        let (left, right) = (40.0 * s, w - 16.0 * s - bars);
        let label = paint::elide(&look.label, right - left, |candidate| text.width(candidate));
        text.draw(
            canvas,
            &label,
            left,
            orb_y + 4.5 * s,
            look::INK,
            0.92 * opacity,
        );
    }
}

/// Returns the width taken, so the label can stop short of the bars.
fn draw_bars(
    canvas: &mut Canvas,
    level: f32,
    seconds: f32,
    color: Rgb,
    opacity: f32,
    s: f32,
) -> f32 {
    let (bar_w, gap, tallest, shortest) = (3.0 * s, 3.0 * s, 18.0 * s, 3.0 * s);
    let total = look::BARS as f32 * bar_w + (look::BARS - 1) as f32 * gap;
    let x0 = WIDTH as f32 * s - 18.0 * s - total;
    let middle = HEIGHT as f32 * s / 2.0;
    for (i, fraction) in look::bar_heights(level, seconds).iter().enumerate() {
        let height = shortest + (tallest - shortest) * fraction;
        let bar = Rect {
            x: x0 + i as f32 * (bar_w + gap),
            y: middle - height / 2.0,
            w: bar_w,
            h: height,
        };
        canvas.rounded_rect(bar, bar_w / 2.0, color, 0.95 * opacity);
    }
    total + 8.0 * s
}

struct Island {
    surface: wl_surface::WlSurface,
    layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
}

impl Island {
    fn map(state: &Wayland, qh: &QueueHandle<Wayland>, at_top: bool) -> Self {
        let compositor = state.compositor.as_ref().expect("checked in connect");
        let shell = state.layer_shell.as_ref().expect("checked in connect");

        let surface = compositor.create_surface(qh, ());
        // "nebula" is the namespace a Hyprland `layerrule` matches on.
        let layer = shell.get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            "nebula".into(),
            qh,
            (),
        );
        layer.set_size(WIDTH, HEIGHT);
        if at_top {
            layer.set_anchor(zwlr_layer_surface_v1::Anchor::Top);
            layer.set_margin(MARGIN, 0, 0, 0);
        } else {
            layer.set_anchor(zwlr_layer_surface_v1::Anchor::Bottom);
            layer.set_margin(0, 0, MARGIN, 0);
        }
        layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);

        let region = compositor.create_region(qh, ());
        region.add(0, 0, WIDTH as i32, HEIGHT as i32);
        surface.set_input_region(Some(&region));
        region.destroy();

        surface.commit();
        Self { surface, layer }
    }
}

impl Drop for Island {
    fn drop(&mut self) {
        self.layer.destroy();
        self.surface.destroy();
    }
}

// wl_buffer.release is required before reuse: https://wayland.freedesktop.org/docs/html/apa.html#protocol-spec-wl_buffer
struct Buffers {
    file: File,
    pool: wl_shm_pool::WlShmPool,
    buffers: [wl_buffer::WlBuffer; 2],
    canvas: Canvas,
    next: usize,
    busy: [Arc<AtomicBool>; 2],
    scale: i32,
}

impl Buffers {
    fn create(shm: &wl_shm::WlShm, qh: &QueueHandle<Wayland>, scale: i32) -> Result<Self> {
        let (width, height) = (WIDTH as i32 * scale, HEIGHT as i32 * scale);
        let stride = width * 4;
        let frame = stride * height;
        let file = shared_memory(frame as usize * 2)?;
        let pool = shm.create_pool(file.as_fd(), frame * 2, qh, ());
        let busy = [
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        ];
        let buffers = [0, 1].map(|slot| {
            pool.create_buffer(
                slot * frame,
                width,
                height,
                stride,
                wl_shm::Format::Argb8888,
                qh,
                busy[slot as usize].clone(),
            )
        });
        Ok(Self {
            file,
            pool,
            buffers,
            canvas: Canvas::new(width as usize, height as usize),
            next: 0,
            busy,
            scale,
        })
    }

    fn present(&mut self, surface: &wl_surface::WlSurface) -> Result<()> {
        let Some(slot) = [self.next, 1 - self.next]
            .into_iter()
            .find(|&slot| !self.busy[slot].load(Ordering::Relaxed))
        else {
            return Ok(());
        };
        self.next = 1 - slot;
        self.busy[slot].store(true, Ordering::Relaxed);
        self.file
            .write_all_at(
                &self.canvas.pixels,
                (slot * self.canvas.pixels.len()) as u64,
            )
            .context("writing a HUD frame")?;
        // Both requests postdate wl_surface v1; a compositor offering less gets
        // an unscaled, fully damaged surface instead of a protocol error.
        if surface.version() >= 3 {
            surface.set_buffer_scale(self.scale);
        }
        surface.attach(Some(&self.buffers[slot]), 0, 0);
        if surface.version() >= 4 {
            surface.damage_buffer(0, 0, self.canvas.width as i32, self.canvas.height as i32);
        } else {
            surface.damage(0, 0, WIDTH as i32, HEIGHT as i32);
        }
        surface.commit();
        Ok(())
    }
}

impl Drop for Buffers {
    fn drop(&mut self) {
        for buffer in &self.buffers {
            buffer.destroy();
        }
        self.pool.destroy();
    }
}

fn shared_memory(size: usize) -> Result<File> {
    // SAFETY: the name is a valid C string; a non-negative result is a new
    // descriptor owned by nothing else.
    let fd = unsafe { libc::memfd_create(c"nebula-hud".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("memfd_create");
    }
    // SAFETY: `fd` was just created above and is not owned elsewhere.
    let file = unsafe { File::from_raw_fd(fd) };
    file.set_len(size as u64).context("sizing the HUD buffer")?;
    Ok(file)
}

impl Dispatch<wl_registry::WlRegistry, ()> for Wayland {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match &interface[..] {
            // Version 6 carries preferred_buffer_scale, which keeps the pill
            // sharp on a scaled output instead of upscaled and soft.
            "wl_compositor" => state.compositor = Some(registry.bind(name, version.min(6), qh, ())),
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "zwlr_layer_shell_v1" => {
                state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
            }
            "wl_seat" => state.seat = Some(registry.bind(name, version.min(7), qh, ())),
            _ => {}
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for Wayland {
    fn event(
        state: &mut Self,
        _: &wl_surface::WlSurface,
        event: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_surface::Event::PreferredBufferScale { factor } = event {
            state.scale = factor.max(1);
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for Wayland {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                layer.ack_configure(serial);
                state.configured = true;
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Wayland {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
            && caps.contains(wl_seat::Capability::Pointer)
            && state.pointer.is_none()
        {
            state.pointer = Some(seat.get_pointer(qh, ()));
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Wayland {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The pill is the only surface this client owns, so any button event
        // the pointer delivers is a click on it.
        if let wl_pointer::Event::Button {
            button,
            state: WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            state.clicks.push(button);
        }
    }
}

delegate_noop!(Wayland: ignore wl_compositor::WlCompositor);
delegate_noop!(Wayland: ignore wl_shm::WlShm);
delegate_noop!(Wayland: ignore wl_shm_pool::WlShmPool);
impl Dispatch<wl_buffer::WlBuffer, Arc<AtomicBool>> for Wayland {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        busy: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            busy.store(false, Ordering::Relaxed);
        }
    }
}
delegate_noop!(Wayland: ignore wl_region::WlRegion);
delegate_noop!(Wayland: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opacity_fades_in_and_out_within_bounds() {
        assert_eq!(step_opacity(0.0, true, 1.0), 1.0);
        assert_eq!(step_opacity(1.0, false, 1.0), 0.0);
        let half_in = step_opacity(0.0, true, FADE_IN / 2.0);
        assert!((half_in - 0.5).abs() < 1e-4);
    }

    #[test]
    fn position_picks_the_edge() {
        assert!(anchored_top("top-center"));
        assert!(!anchored_top("bottom-center"));
        assert!(!anchored_top("anything else"));
    }

    #[test]
    fn every_status_draws_without_a_font() {
        let mut canvas = Canvas::new(WIDTH as usize, HEIGHT as usize);
        for status in [
            UiStatus::Standby,
            UiStatus::Connecting,
            UiStatus::Listening { level: 0.7 },
            UiStatus::Thinking,
            UiStatus::Speaking { level: 0.4 },
            UiStatus::Muted,
            UiStatus::Error {
                message: "offline".into(),
            },
        ] {
            draw(&mut canvas, &status, None, 0.5, 1.0, 1.0, 1.0);
            assert!(
                canvas.pixels.iter().any(|&b| b > 0),
                "{status:?} drew nothing"
            );
        }
    }
}
