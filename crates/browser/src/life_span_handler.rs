//! CEF Life Span Handler
//!
//! Handles browser lifecycle events: popup requests, browser creation,
//! and browser close. Popup-based login flows rely on real popup windows,
//! so we allow popup creation instead of rewriting to tab navigation.

use crate::client::ClientBuilder;
use crate::events::{BrowserEvent, EventSender, OpenDisposition, OpenTargetRequest};
use crate::render_handler::RenderState;
use cef::{
    Browser, ImplLifeSpanHandler, LifeSpanHandler, WrapLifeSpanHandler, rc::Rc as _,
    wrap_life_span_handler,
};
#[cfg(target_os = "macos")]
use core_video::pixel_buffer::CVPixelBuffer;
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Clone)]
pub struct OsrLifeSpanHandler {
    sender: EventSender,
}

impl OsrLifeSpanHandler {
    pub fn new(sender: EventSender) -> Self {
        Self { sender }
    }

    fn popup_client() -> cef::Client {
        let render_state = Arc::new(Mutex::new(RenderState::default()));
        #[cfg(target_os = "macos")]
        let current_frame: Arc<Mutex<Option<CVPixelBuffer>>> = Arc::new(Mutex::new(None));
        let (popup_sender, _popup_receiver) = crate::events::event_channel();
        ClientBuilder::build_for_popup(
            render_state,
            #[cfg(target_os = "macos")]
            current_frame,
            popup_sender,
        )
    }
}

wrap_life_span_handler! {
    pub struct LifeSpanHandlerBuilder {
        handler: OsrLifeSpanHandler,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut cef::Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&cef::CefString>,
            _target_frame_name: Option<&cef::CefString>,
            target_disposition: cef::WindowOpenDisposition,
            user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&cef::PopupFeatures>,
            window_info: Option<&mut cef::WindowInfo>,
            client: Option<&mut Option<cef::Client>>,
            _settings: Option<&mut cef::BrowserSettings>,
            _extra_info: Option<&mut Option<cef::DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let disposition = OpenDisposition::from(target_disposition);
            let target_url = target_url.map(ToString::to_string).filter(|url| !url.is_empty());

            if let Some(tab_target) = disposition.app_tab_target()
                && let Some(target_url) = target_url
            {
                let request = OpenTargetRequest {
                    url: target_url,
                    disposition,
                    user_gesture: user_gesture != 0,
                    is_popup_request: true,
                };

                if let Err(error) = self
                    .handler
                    .sender
                    .send(BrowserEvent::OpenTargetRequested(request))
                {
                    log::debug!("[browser] failed to send popup open target request: {}", error);
                    return 0;
                }

                log::info!(
                    "[browser] redirecting popup disposition {:?} into browser tab flow",
                    tab_target,
                );
                return 1;
            }

            if !disposition.allow_native_popup() {
                return 0;
            }

            // Ensure popup is hosted as a real window and not as an off-screen
            // rendering child tied to the parent tab's event/render pipeline.
            if let Some(window_info) = window_info {
                window_info.windowless_rendering_enabled = 0;
                window_info.shared_texture_enabled = 0;
            }

            if let Some(client) = client {
                *client = Some(OsrLifeSpanHandler::popup_client());
            }

            if let Some(url) = target_url {
                log::info!("[browser] allowing native popup navigation: {}", url);
            }

            0 // Allow popup creation.
        }

        fn on_after_created(&self, _browser: Option<&mut Browser>) {
            let _ = self.handler.sender.send(BrowserEvent::BrowserCreated);
        }

        fn do_close(&self, _browser: Option<&mut Browser>) -> ::std::os::raw::c_int {
            0 // Allow close
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
        }
    }
}

impl LifeSpanHandlerBuilder {
    pub fn build(handler: OsrLifeSpanHandler) -> cef::LifeSpanHandler {
        Self::new(handler)
    }
}
