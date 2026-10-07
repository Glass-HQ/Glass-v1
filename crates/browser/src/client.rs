//! CEF Client Implementation
//!
//! Provides the Client that CEF uses to communicate with the browser.
//! Ties together the render, load, display, life span, and keyboard handlers.

#[cfg(target_os = "windows")]
use cef::sys::tagMSG;
use cef::{
    Browser, Client, ContextMenuHandler, DisplayHandler, DownloadHandler, FindHandler, ImplClient,
    ImplKeyboardHandler, KeyEvent, KeyboardHandler, LifeSpanHandler, LoadHandler,
    PermissionHandler, RenderHandler, WrapClient, WrapKeyboardHandler, rc::Rc as _, wrap_client,
    wrap_keyboard_handler,
};

use crate::context_menu_handler::{ContextMenuHandlerBuilder, OsrContextMenuHandler};
use crate::display_handler::{DisplayHandlerBuilder, OsrDisplayHandler};
use crate::download_handler::{DownloadHandlerBuilder, OsrDownloadHandler};
use crate::events::EventSender;
use crate::find_handler::{FindHandlerBuilder, OsrFindHandler};
use crate::life_span_handler::{LifeSpanHandlerBuilder, OsrLifeSpanHandler};
use crate::load_handler::{LoadHandlerBuilder, OsrLoadHandler};
use crate::page_chrome::extract_page_chrome_from_message;
use crate::permission_handler::{OsrPermissionHandler, PermissionHandlerBuilder};
use crate::render_handler::{OsrRenderHandler, RenderHandlerBuilder, RenderState};
use crate::request_handler::{OsrRequestHandler, RequestHandlerBuilder};
use crate::text_input::extract_text_input_state_from_message;
#[cfg(target_os = "macos")]
use core_video::pixel_buffer::CVPixelBuffer;
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ── Keyboard Handler ─────────────────────────────────────────────────
// Off-screen browser views receive input through Glass routing only:
// raw key events are forwarded explicitly for browser-owned keys and text input
// is committed through the IME APIs. Native AppKit events would duplicate that.

pub(crate) static MANUAL_KEY_EVENT: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "windows")]
type KeyboardOsEvent<'a> = Option<&'a mut tagMSG>;
#[cfg(not(target_os = "windows"))]
type KeyboardOsEvent<'a> = *mut u8;

#[derive(Clone)]
struct OsrKeyboardHandler;

wrap_keyboard_handler! {
    struct KeyboardHandlerBuilder {
        handler: OsrKeyboardHandler,
    }

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            _browser: Option<&mut Browser>,
            _event: Option<&KeyEvent>,
            _os_event: KeyboardOsEvent<'_>,
            _is_keyboard_shortcut: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let is_manual = MANUAL_KEY_EVENT.load(Ordering::Relaxed);
            if is_manual { 0 } else { 1 }
        }
    }
}

impl KeyboardHandlerBuilder {
    fn build() -> cef::KeyboardHandler {
        Self::new(OsrKeyboardHandler)
    }
}

// Popup windows are real native windows, so their key events come through
// the OS natively and should NOT be suppressed. This handler allows all
// events through.
#[derive(Clone)]
struct PopupKeyboardHandler;

wrap_keyboard_handler! {
    struct PopupKeyboardHandlerBuilder {
        handler: PopupKeyboardHandler,
    }

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            _browser: Option<&mut Browser>,
            _event: Option<&KeyEvent>,
            _os_event: KeyboardOsEvent<'_>,
            _is_keyboard_shortcut: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            0 // Allow all native key events through.
        }
    }
}

impl PopupKeyboardHandlerBuilder {
    fn build() -> cef::KeyboardHandler {
        Self::new(PopupKeyboardHandler)
    }
}

// ── Client ───────────────────────────────────────────────────────────

wrap_client! {
    pub struct ClientBuilder {
        render_handler: RenderHandler,
        load_handler: LoadHandler,
        display_handler: DisplayHandler,
        life_span_handler: LifeSpanHandler,
        keyboard_handler: KeyboardHandler,
        download_handler: DownloadHandler,
        find_handler: FindHandler,
        request_handler: cef::RequestHandler,
    context_menu_handler: ContextMenuHandler,
    permission_handler: PermissionHandler,
    event_sender: EventSender,
    }

    impl Client {
        fn render_handler(&self) -> Option<cef::RenderHandler> {
            Some(self.render_handler.clone())
        }

        fn load_handler(&self) -> Option<cef::LoadHandler> {
            Some(self.load_handler.clone())
        }

        fn display_handler(&self) -> Option<cef::DisplayHandler> {
            Some(self.display_handler.clone())
        }

        fn life_span_handler(&self) -> Option<cef::LifeSpanHandler> {
            Some(self.life_span_handler.clone())
        }

        fn keyboard_handler(&self) -> Option<cef::KeyboardHandler> {
            Some(self.keyboard_handler.clone())
        }

        fn download_handler(&self) -> Option<cef::DownloadHandler> {
            Some(self.download_handler.clone())
        }

        fn find_handler(&self) -> Option<cef::FindHandler> {
            Some(self.find_handler.clone())
        }

        fn request_handler(&self) -> Option<cef::RequestHandler> {
            Some(self.request_handler.clone())
        }

        fn context_menu_handler(&self) -> Option<cef::ContextMenuHandler> {
            Some(self.context_menu_handler.clone())
        }

        fn permission_handler(&self) -> Option<cef::PermissionHandler> {
            Some(self.permission_handler.clone())
        }

        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut cef::Frame>,
            _source_process: cef::ProcessId,
            message: Option<&mut cef::ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let Some(message) = message else {
                return 0;
            };

            if let Some(text_input_state) = extract_text_input_state_from_message(message) {
                let _ = self
                    .event_sender
                    .send(crate::events::BrowserEvent::TextInputStateChanged(
                        text_input_state,
                    ));
                return 1;
            }

            let Some(page_chrome) = extract_page_chrome_from_message(message) else {
                return 0;
            };

            let _ = self
                .event_sender
                .send(crate::events::BrowserEvent::PageChromeChanged(page_chrome));
            1
        }
    }
}

impl ClientBuilder {
    pub fn build(
        render_state: Arc<Mutex<RenderState>>,
        #[cfg(target_os = "macos")] current_frame: Arc<Mutex<Option<CVPixelBuffer>>>,
        event_sender: EventSender,
    ) -> cef::Client {
        Self::build_inner(
            render_state,
            #[cfg(target_os = "macos")]
            current_frame,
            event_sender,
            KeyboardHandlerBuilder::build(),
        )
    }

    pub fn build_for_popup(
        render_state: Arc<Mutex<RenderState>>,
        #[cfg(target_os = "macos")] current_frame: Arc<Mutex<Option<CVPixelBuffer>>>,
        event_sender: EventSender,
    ) -> cef::Client {
        Self::build_inner(
            render_state,
            #[cfg(target_os = "macos")]
            current_frame,
            event_sender,
            PopupKeyboardHandlerBuilder::build(),
        )
    }

    fn build_inner(
        render_state: Arc<Mutex<RenderState>>,
        #[cfg(target_os = "macos")] current_frame: Arc<Mutex<Option<CVPixelBuffer>>>,
        event_sender: EventSender,
        keyboard_handler: cef::KeyboardHandler,
    ) -> cef::Client {
        let render_handler = OsrRenderHandler::new(
            render_state,
            #[cfg(target_os = "macos")]
            current_frame,
            event_sender.clone(),
        );
        let load_handler = OsrLoadHandler::new(event_sender.clone());
        let display_handler = OsrDisplayHandler::new(event_sender.clone());
        let life_span_handler = OsrLifeSpanHandler::new(event_sender.clone());
        let request_handler = OsrRequestHandler::new(event_sender.clone());
        let download_handler = OsrDownloadHandler::new(event_sender.clone());
        let find_handler = OsrFindHandler::new(event_sender.clone());
        let context_menu_handler = OsrContextMenuHandler::new(event_sender.clone());
        let permission_handler = OsrPermissionHandler::new();
        Self::new(
            RenderHandlerBuilder::build(render_handler),
            LoadHandlerBuilder::build(load_handler),
            DisplayHandlerBuilder::build(display_handler),
            LifeSpanHandlerBuilder::build(life_span_handler),
            keyboard_handler,
            DownloadHandlerBuilder::build(download_handler),
            FindHandlerBuilder::build(find_handler),
            RequestHandlerBuilder::build(request_handler),
            ContextMenuHandlerBuilder::build(context_menu_handler),
            PermissionHandlerBuilder::build(permission_handler),
            event_sender,
        )
    }
}
