//! CEF Render Handler
//!
//! Implements CEF's RenderHandler trait to capture off-screen rendered frames.
//! When shared_texture_enabled is set, CEF calls on_accelerated_paint with an
//! IOSurface handle. We wrap it as a CVPixelBuffer for zero-copy rendering
//! through GPUI's Surface element.

use crate::events::{BrowserEvent, EventSender};
use cef::{
    AcceleratedPaintInfo, Browser, ImplRenderHandler, PaintElementType, Rect, RenderHandler,
    ScreenInfo, WrapRenderHandler, rc::Rc as _, wrap_render_handler,
};
#[cfg(target_os = "macos")]
use core_foundation::base::TCFType;
#[cfg(target_os = "macos")]
use core_video::pixel_buffer::CVPixelBuffer;
#[cfg(target_os = "macos")]
#[allow(deprecated)]
use io_surface::IOSurface;
use parking_lot::Mutex;
use std::sync::Arc;

/// Viewport geometry shared between the GPUI resize path and CEF's view_rect/screen_info callbacks.
/// Kept separate from the frame buffer so geometry reads never contend with 60fps frame writes.
pub struct RenderState {
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            width: 800,
            height: 600,
            scale_factor: 1.0,
        }
    }
}

#[derive(Clone)]
pub struct OsrRenderHandler {
    state: Arc<Mutex<RenderState>>,
    #[cfg(target_os = "macos")]
    current_frame: Arc<Mutex<Option<CVPixelBuffer>>>,
    #[cfg(target_os = "macos")]
    sender: EventSender,
}

impl OsrRenderHandler {
    pub fn new(
        state: Arc<Mutex<RenderState>>,
        #[cfg(target_os = "macos")] current_frame: Arc<Mutex<Option<CVPixelBuffer>>>,
        sender: EventSender,
    ) -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                state,
                current_frame,
                sender,
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = sender;
            Self { state }
        }
    }
}

wrap_render_handler! {
    pub struct RenderHandlerBuilder {
        handler: OsrRenderHandler,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let state = self.handler.state.lock();
                rect.x = 0;
                rect.y = 0;
                rect.width = state.width as i32;
                rect.height = state.height as i32;
            }
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            if let Some(info) = screen_info {
                let state = self.handler.state.lock();
                info.device_scale_factor = state.scale_factor;
                info.rect.x = 0;
                info.rect.y = 0;
                info.rect.width = state.width as i32;
                info.rect.height = state.height as i32;
                info.available_rect = info.rect.clone();
                info.depth = 32;
                info.depth_per_component = 8;
                info.is_monochrome = 0;
                return 1;
            }
            0
        }

        fn screen_point(
            &self,
            _browser: Option<&mut Browser>,
            view_x: ::std::os::raw::c_int,
            view_y: ::std::os::raw::c_int,
            screen_x: Option<&mut ::std::os::raw::c_int>,
            screen_y: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            if let Some(screen_x) = screen_x {
                *screen_x = view_x;
            }
            if let Some(screen_y) = screen_y {
                *screen_y = view_y;
            }
            1
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            if type_ != PaintElementType::default() {
                return;
            }

            #[cfg(target_os = "macos")]
            {
                let Some(info) = info else {
                    log::warn!("[browser::render_handler] on_accelerated_paint() no info");
                    return;
                };

                let io_surface_ptr = info.shared_texture_io_surface;
                if io_surface_ptr.is_null() {
                    log::warn!("[browser::render_handler] on_accelerated_paint() null IOSurface");
                    return;
                }

                #[allow(deprecated)]
                let io_surface: IOSurface = unsafe {
                    TCFType::wrap_under_get_rule(io_surface_ptr as io_surface::IOSurfaceRef)
                };

                let pixel_buffer = match CVPixelBuffer::from_io_surface(&io_surface, None) {
                    Ok(pb) => pb,
                    Err(err) => {
                        log::error!("[browser::render_handler] on_accelerated_paint() CVPixelBuffer::from_io_surface failed: {:?}", err);
                        return;
                    }
                };

                *self.handler.current_frame.lock() = Some(pixel_buffer);
                let _ = self.handler.sender.send(BrowserEvent::FrameReady);
            }

            #[cfg(not(target_os = "macos"))]
            {
                let _ = info;
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            _type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            _buffer: *const u8,
            _width: ::std::os::raw::c_int,
            _height: ::std::os::raw::c_int,
        ) {
            // Fallback: should not be called when shared_texture_enabled is set.
            log::warn!("[browser::render_handler] on_paint() called unexpectedly (shared_texture_enabled should prevent this)");
        }
    }
}

impl RenderHandlerBuilder {
    pub fn build(handler: OsrRenderHandler) -> cef::RenderHandler {
        Self::new(handler)
    }
}
