use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::rc::Rc;

use softbuffer::{Context as SbContext, Surface as SbSurface};
use tokio::sync::mpsc;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, MouseScrollDelta, WindowEvent},
    keyboard::{ModifiersState, PhysicalKey},
    window::Window,
};

use crate::hid::keycode_to_hid;

#[derive(Debug)]
pub enum AppCmd {
    Mouse {
        buttons: u8,
        dx: i8,
        dy: i8,
        wheel: i8,
    },
    KeyDown(u8),
    KeyUp(u8),
    Battery(u8),
}

pub struct App {
    window: Option<Rc<Window>>,
    sb_ctx: Option<SbContext<Rc<Window>>>,
    sb_surface: Option<SbSurface<Rc<Window>, Rc<Window>>>,
    cmd_tx: mpsc::Sender<AppCmd>,
    mouse_buttons: u8,
    cursor_last: Option<(f64, f64)>,
    wheel_px_accum: f64,
    mods_winit: ModifiersState,
    hid_mod_mask: u8,
    pressed_usages: BTreeSet<u8>,
    size: PhysicalSize<u32>,
    exiting: bool,
}

impl App {
    pub fn new(cmd_tx: mpsc::Sender<AppCmd>) -> Self {
        Self {
            window: None,
            sb_ctx: None,
            sb_surface: None,
            cmd_tx,
            mouse_buttons: 0,
            cursor_last: None,
            wheel_px_accum: 0.0,
            mods_winit: ModifiersState::empty(),
            hid_mod_mask: 0,
            pressed_usages: BTreeSet::new(),
            size: PhysicalSize::new(800, 600),
            exiting: false,
        }
    }

    #[inline]
    fn send(&self, cmd: AppCmd) {
        let _ = self.cmd_tx.try_send(cmd);
    }

    fn set_button(&mut self, button: winit::event::MouseButton, pressed: bool) {
        let bit = match button {
            winit::event::MouseButton::Left => 0,
            winit::event::MouseButton::Middle => 1,
            winit::event::MouseButton::Right => 2,
            _ => return,
        };
        if pressed {
            self.mouse_buttons |= 1 << bit;
        } else {
            self.mouse_buttons &= !(1 << bit);
        }
    }

    fn send_mouse(&self, dx: f64, dy: f64, wheel: i32) {
        let clamp = |v: f64| v.clamp(i8::MIN as f64, i8::MAX as f64) as i8;
        self.send(AppCmd::Mouse {
            buttons: self.mouse_buttons,
            dx: clamp(dx),
            dy: clamp(dy),
            wheel: wheel.clamp(i8::MIN as i32, i8::MAX as i32) as i8,
        });
    }

    fn note_modifier_physical_transition(&mut self, usage: u8, down: bool) {
        let bit = match usage {
            0xE0 => 0,
            0xE1 => 1,
            0xE2 => 2,
            0xE3 => 3,
            0xE4 => 4,
            0xE5 => 5,
            0xE6 => 6,
            0xE7 => 7,
            _ => return,
        };
        if down {
            self.hid_mod_mask |= 1 << bit;
        } else {
            self.hid_mod_mask &= !(1 << bit);
        }
    }

    fn draw_once_black(&mut self) {
        // Lazy init if needed
        if self.sb_surface.is_none() {
            if let Some(win_ref) = self.window.as_ref() {
                let win_owned = Rc::clone(win_ref);
                if self.sb_ctx.is_none() {
                    match SbContext::new(win_owned.clone()) {
                        Ok(c) => self.sb_ctx = Some(c),
                        Err(e) => {
                            tracing::error!(error = %e, "softbuffer context error");
                            return;
                        }
                    }
                }
                if let Some(ctx) = self.sb_ctx.as_ref() {
                    match SbSurface::new(ctx, win_owned.clone()) {
                        Ok(s) => self.sb_surface = Some(s),
                        Err(e) => {
                            tracing::error!(error = %e, "softbuffer surface error");
                            return;
                        }
                    }
                }
            } else {
                return;
            }
        }
        let w = NonZeroU32::new(self.size.width.max(1)).unwrap();
        let h = NonZeroU32::new(self.size.height.max(1)).unwrap();
        if let Some(surf) = self.sb_surface.as_mut() {
            if let Err(e) = surf.resize(w, h) {
                tracing::error!(error = %e, "softbuffer resize error");
                return;
            }
            if let Ok(mut buf) = surf.buffer_mut() {
                buf.fill(0xFF00_0000);
                if let Err(e) = buf.present() {
                    tracing::error!(error = %e, "softbuffer present error");
                }
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &winit::event_loop::ActiveEventLoop) {
        if self.exiting {
            return;
        }
        let win = el.create_window(Window::default_attributes()).unwrap();
        self.size = win.inner_size();
        self.window = Some(Rc::new(win));
        self.window.as_ref().unwrap().request_redraw();
        tracing::info!("[winit] resumed -> window created");
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        tracing::trace!(?event, "winit event");
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                self.exiting = true;
                // Drop softbuffer resources explicitly
                self.sb_surface = None;
                self.sb_ctx = None;
                event_loop.exit();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let usage = match &event.physical_key {
                    PhysicalKey::Code(code) => keycode_to_hid(*code),
                    _ => None,
                };
                if let Some(u) = usage {
                    let down = matches!(event.state, ElementState::Pressed);
                    // Track pressed usages for focus-loss cleanup
                    if down {
                        self.pressed_usages.insert(u);
                    } else {
                        self.pressed_usages.remove(&u);
                    }
                    self.send(if down {
                        AppCmd::KeyDown(u)
                    } else {
                        AppCmd::KeyUp(u)
                    });
                    self.note_modifier_physical_transition(u, down);
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                // Record for UI state only; don't reconcile to avoid double-sends
                self.mods_winit = m.state();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.set_button(button, matches!(state, ElementState::Pressed));
                self.send_mouse(0.0, 0.0, 0);
            }
            WindowEvent::CursorEntered { .. } | WindowEvent::CursorLeft { .. } => {
                self.cursor_last = None;
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = (position.x, position.y);
                if let Some((px, py)) = self.cursor_last.replace((x, y)) {
                    self.send_mouse(x - px, y - py, 0);
                } else {
                    self.send_mouse(0.0, 0.0, 0);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                const PX_PER_NOTCH: f64 = 120.0;
                let mut notches = 0i32;
                match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        notches = y.round() as i32;
                    }
                    MouseScrollDelta::PixelDelta(p) => {
                        self.wheel_px_accum += p.y;
                        while self.wheel_px_accum.abs() >= PX_PER_NOTCH {
                            if self.wheel_px_accum > 0.0 {
                                notches += 1;
                                self.wheel_px_accum -= PX_PER_NOTCH;
                            } else {
                                notches -= 1;
                                self.wheel_px_accum += PX_PER_NOTCH;
                            }
                        }
                    }
                }
                if notches != 0 {
                    self.send_mouse(0.0, 0.0, notches);
                }
            }
            WindowEvent::Focused(focused) => {
                if !focused {
                    // Send key up for all pressed usages and clear modifiers
                    for &u in self.pressed_usages.clone().iter() {
                        self.send(AppCmd::KeyUp(u));
                    }
                    self.pressed_usages.clear();
                    self.hid_mod_mask = 0;
                }
                tracing::info!(%focused, "Focused");
            }
            WindowEvent::Resized(sz) => {
                self.size = sz;
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                self.draw_once_black();
            }
            _ => {}
        }
    }
}
