//! Browser Tab Entity
//!
//! GPUI Entity wrapping a CEF Browser instance. Owns all navigation state
//! and drains the event channel from CEF handlers to emit GPUI events.
//!
//! CEF browser handles are stored in a centralized global registry
//! (`BROWSER_HANDLES`) rather than directly on BrowserTab. This allows
//! CEF shutdown to take all handles, close browsers, and release ref
//! counts before calling `cef::shutdown()` — regardless of whether
//! GPUI has dropped the BrowserTab entities yet.

use crate::client::ClientBuilder;
use crate::context_menu_handler::ContextMenuContext;
use crate::events::{
    self, BrowserEvent, DownloadUpdatedEvent, EventReceiver, FindResultEvent, OpenTargetRequest,
};
use crate::page_chrome::PageChrome;
use crate::render_handler::RenderState;
use crate::text_input::BrowserTextInputState;
use anyhow::{Context as _, Result};
use cef::{
    ImplBrowser, ImplBrowserHost, ImplFrame, ImplRequestContext, KeyEvent, KeyEventType,
    MouseButtonType,
};
#[cfg(target_os = "macos")]
use core_video::pixel_buffer::CVPixelBuffer;
use gpui::{Context, EventEmitter, Hsla};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
/// All live CEF browser handles, keyed by browser ID.
///
/// BrowserTab stores only the integer ID and accesses the handle through
/// this map. During shutdown, `close_all_browsers()` takes the entire map,
/// force-closes every browser, and drops the handles — releasing all CEF
/// ref counts before `cef::shutdown()` is called.
static BROWSER_HANDLES: Mutex<Option<HashMap<i32, cef::Browser>>> = Mutex::new(None);

/// Force-close all tracked browsers and release their CEF handles.
///
/// This takes every handle out of the global map, calls
/// `host.close_browser(force_close=1)` on each, then drops them all.
/// After this returns, no Rust code holds `cef::Browser` references,
/// so CEF's internal `BrowserContext` ref counts can reach zero.
pub(crate) fn close_all_browsers() -> usize {
    let handles = BROWSER_HANDLES.lock().take().unwrap_or_default();
    let count = handles.len();
    if count > 0 {
        log::trace!(
            "[browser::tab] close_all_browsers: closing {} browser(s)",
            count,
        );
    }
    for (id, browser) in handles {
        let is_valid = browser.is_valid();
        match browser.host() {
            Some(host) => {
                let ready_to_close = host.is_ready_to_be_closed();
                log::trace!(
                    "[browser::tab] close_all_browsers: id={} is_valid={} ready_to_close={}, requesting close",
                    id,
                    is_valid,
                    ready_to_close,
                );
                host.close_browser(1);
            }
            None => {
                log::trace!(
                    "[browser::tab] close_all_browsers: id={} is_valid={} no host",
                    id,
                    is_valid,
                );
            }
        }
    }
    count
}

/// Events emitted by BrowserTab to subscribers (toolbar, browser_view).
pub(crate) enum TabEvent {
    AddressChanged(String),
    TitleChanged,
    LoadingStateChanged,
    PageChromeChanged,
    TextInputStateChanged(BrowserTextInputState),
    #[cfg(target_os = "macos")]
    FrameReady,
    NavigateToUrl(String),
    OpenNewTab(String),
    OpenTargetRequested(OpenTargetRequest),
    FaviconChanged,
    LoadError {
        url: String,
        error_text: String,
    },
    ContextMenuOpen {
        context: ContextMenuContext,
    },
    FindResult(FindResultEvent),
    DownloadUpdated(DownloadUpdatedEvent),
}

pub struct BrowserTab {
    browser_id: Option<i32>,
    client: cef::Client,
    render_state: Arc<Mutex<RenderState>>,
    #[cfg(target_os = "macos")]
    current_frame: Arc<Mutex<Option<CVPixelBuffer>>>,
    event_receiver: EventReceiver,
    url: String,
    title: String,
    is_loading: bool,
    can_go_back: bool,
    can_go_forward: bool,
    loading_progress: f64,
    is_new_tab_page: bool,
    is_pinned: bool,
    favicon_url: Option<String>,
    page_chrome: Option<PageChrome>,
    text_input_state: BrowserTextInputState,
    pending_url: Option<String>,
    suspended_url: Option<String>,
    request_context: Option<cef::RequestContext>,
}

impl EventEmitter<TabEvent> for BrowserTab {}

impl BrowserTab {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        let render_state = Arc::new(Mutex::new(RenderState::default()));
        #[cfg(target_os = "macos")]
        let current_frame: Arc<Mutex<Option<CVPixelBuffer>>> = Arc::new(Mutex::new(None));
        let (sender, receiver) = events::event_channel();
        let client = ClientBuilder::build(
            render_state.clone(),
            #[cfg(target_os = "macos")]
            current_frame.clone(),
            sender,
        );

        Self {
            browser_id: None,
            client,
            render_state,
            #[cfg(target_os = "macos")]
            current_frame,
            event_receiver: receiver,
            url: String::from("glass://newtab"),
            title: String::from("New Tab"),
            is_loading: false,
            can_go_back: false,
            can_go_forward: false,
            loading_progress: 0.0,
            is_new_tab_page: true,
            is_pinned: false,
            favicon_url: None,
            page_chrome: None,
            text_input_state: BrowserTextInputState::default(),
            pending_url: None,
            suspended_url: None,
            request_context: None,
        }
    }

    pub fn new_with_state(
        url: String,
        title: String,
        is_new_tab_page: bool,
        favicon_url: Option<String>,
        _cx: &mut Context<Self>,
    ) -> Self {
        let render_state = Arc::new(Mutex::new(RenderState::default()));
        #[cfg(target_os = "macos")]
        let current_frame: Arc<Mutex<Option<CVPixelBuffer>>> = Arc::new(Mutex::new(None));
        let (sender, receiver) = events::event_channel();
        let client = ClientBuilder::build(
            render_state.clone(),
            #[cfg(target_os = "macos")]
            current_frame.clone(),
            sender,
        );

        Self {
            browser_id: None,
            client,
            render_state,
            #[cfg(target_os = "macos")]
            current_frame,
            event_receiver: receiver,
            url,
            title,
            is_loading: false,
            can_go_back: false,
            can_go_forward: false,
            loading_progress: 0.0,
            is_new_tab_page,
            is_pinned: false,
            favicon_url,
            page_chrome: None,
            text_input_state: BrowserTextInputState::default(),
            pending_url: None,
            suspended_url: None,
            request_context: None,
        }
    }

    pub fn drain_events(&mut self, cx: &mut Context<Self>) {
        let is_suspended = self.suspended_url.is_some();
        while let Ok(event) = self.event_receiver.try_recv() {
            match event {
                BrowserEvent::AddressChanged(url) => {
                    // While suspended, ignore address changes from the hidden page
                    if is_suspended {
                        continue;
                    }
                    self.page_chrome = None;
                    if self.text_input_state != BrowserTextInputState::default() {
                        self.text_input_state = BrowserTextInputState::default();
                        cx.emit(TabEvent::TextInputStateChanged(self.text_input_state));
                    }
                    self.url.clone_from(&url);
                    cx.emit(TabEvent::AddressChanged(url));
                    cx.emit(TabEvent::PageChromeChanged);
                }
                BrowserEvent::TitleChanged(title) => {
                    if is_suspended {
                        continue;
                    }
                    self.title.clone_from(&title);
                    cx.emit(TabEvent::TitleChanged);
                }
                BrowserEvent::LoadingStateChanged {
                    is_loading,
                    can_go_back,
                    can_go_forward,
                } => {
                    self.is_loading = is_loading;
                    self.can_go_back = can_go_back;
                    self.can_go_forward = can_go_forward;
                    cx.emit(TabEvent::LoadingStateChanged);
                }
                BrowserEvent::LoadingProgress(progress) => {
                    self.loading_progress = progress;
                }
                #[cfg(target_os = "macos")]
                BrowserEvent::FrameReady => {
                    if !is_suspended {
                        cx.emit(TabEvent::FrameReady);
                    }
                }
                BrowserEvent::BrowserCreated => {}
                BrowserEvent::LoadError { url, error_text } => {
                    cx.emit(TabEvent::LoadError { url, error_text });
                }
                BrowserEvent::ContextMenuRequested { context } => {
                    cx.emit(TabEvent::ContextMenuOpen { context });
                }
                BrowserEvent::OpenTargetRequested(request) => {
                    cx.emit(TabEvent::OpenTargetRequested(request));
                }
                BrowserEvent::FaviconUrlChanged(urls) => {
                    if is_suspended {
                        continue;
                    }
                    self.favicon_url = urls.into_iter().next();
                    cx.emit(TabEvent::FaviconChanged);
                }
                BrowserEvent::PageChromeChanged(page_chrome) => {
                    if is_suspended {
                        continue;
                    }
                    if self.page_chrome != page_chrome {
                        self.page_chrome = page_chrome;
                        cx.emit(TabEvent::PageChromeChanged);
                    }
                }
                BrowserEvent::TextInputStateChanged(text_input_state) => {
                    if is_suspended {
                        continue;
                    }

                    if self.text_input_state != text_input_state {
                        self.text_input_state = text_input_state;
                        cx.emit(TabEvent::TextInputStateChanged(text_input_state));
                    }
                }
                BrowserEvent::FindResult(result) => {
                    cx.emit(TabEvent::FindResult(result));
                }
                BrowserEvent::DownloadUpdated(update) => {
                    cx.emit(TabEvent::DownloadUpdated(update));
                }
            }
        }
    }

    pub fn create_browser(&mut self, initial_url: &str) -> Result<()> {
        if self.browser_id.is_some() {
            log::trace!(
                "[browser::tab] create_browser: SKIPPED (already has browser), url={}",
                initial_url,
            );
            return Ok(());
        }

        let window_info = cef::WindowInfo {
            windowless_rendering_enabled: 1,
            shared_texture_enabled: 1,
            ..Default::default()
        };

        let browser_settings = cef::BrowserSettings {
            windowless_frame_rate: 60,
            ..Default::default()
        };

        let url = cef::CefString::from(initial_url);

        let mut request_context = self.request_context.clone();

        let browser = cef::browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut self.client.clone()),
            Some(&url),
            Some(&browser_settings),
            None,
            request_context.as_mut(),
        )
        .context("Failed to create CEF browser")?;

        let browser_id = browser.identifier();

        // Diagnostic: verify the browser's actual request context
        if let Some(host) = browser.host() {
            if let Some(context) = host.request_context() {
                let is_global = context.is_global() != 0;
                let has_cookie_manager = context.cookie_manager(None).is_some();
                log::trace!(
                    "[browser::tab] Created browser id={} url={} | context: is_global={}, has_cookie_manager={}",
                    browser_id,
                    initial_url,
                    is_global,
                    has_cookie_manager,
                );
            } else {
                log::warn!(
                    "[browser::tab] Created browser id={} url={} | WARNING: no request context!",
                    browser_id,
                    initial_url,
                );
            }
        } else {
            log::warn!(
                "[browser::tab] Created browser id={} url={} | WARNING: no host!",
                browser_id,
                initial_url,
            );
        }

        BROWSER_HANDLES
            .lock()
            .get_or_insert_with(HashMap::new)
            .insert(browser_id, browser);
        self.browser_id = Some(browser_id);

        self.url = initial_url.to_string();

        self.with_host(|host| {
            host.was_resized();
        });

        Ok(())
    }

    pub fn navigate(&mut self, url: &str, cx: &mut Context<Self>) {
        self.favicon_url = None;
        if self.browser_id.is_some() {
            let loaded = self
                .with_browser(|browser| {
                    if let Some(frame) = browser.main_frame() {
                        let url_string = cef::CefString::from(url);
                        frame.load_url(Some(&url_string));
                        return true;
                    }
                    false
                })
                .unwrap_or(false);

            if loaded {
                self.url = url.to_string();
                self.is_loading = true;
            }
        } else {
            self.url = url.to_string();
            cx.emit(TabEvent::NavigateToUrl(url.to_string()));
        }
    }

    pub fn has_browser(&self) -> bool {
        self.browser_id.is_some()
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended_url.is_some()
    }

    pub fn suspend(&mut self) {
        if self.suspended_url.is_some() {
            return;
        }

        let browser_id = self.browser_id;
        let handle_exists = browser_id
            .and_then(|id| BROWSER_HANDLES.lock().as_ref().map(|m| m.contains_key(&id)))
            .unwrap_or(false);

        log::trace!(
            "[browser::tab] SUSPEND: browser_id={:?}, url={}, handle_in_registry={}",
            browser_id,
            self.url,
            handle_exists,
        );

        self.suspended_url = Some(self.url.clone());

        // Just hide and mute — leave the page fully loaded so all cookies,
        // localStorage, sessionStorage, and JS state are preserved intact.
        self.set_hidden(true);
        self.set_audio_muted(true);
    }

    pub fn resume(&mut self) {
        let Some(url) = self.suspended_url.take() else {
            return;
        };

        let browser_id = self.browser_id;
        let handle_exists = browser_id
            .and_then(|id| BROWSER_HANDLES.lock().as_ref().map(|m| m.contains_key(&id)))
            .unwrap_or(false);

        // Verify the browser's request context is still valid
        let context_info = self
            .with_browser(|browser| {
                browser.host().and_then(|host| {
                    host.request_context().map(|ctx| {
                        let is_global = ctx.is_global() != 0;
                        let has_cookies = ctx.cookie_manager(None).is_some();
                        (is_global, has_cookies)
                    })
                })
            })
            .flatten();

        log::trace!(
            "[browser::tab] RESUME: browser_id={:?}, url={}, handle_in_registry={}, context={:?}",
            browser_id,
            url,
            handle_exists,
            context_info,
        );

        if !handle_exists {
            log::error!(
                "[browser::tab] RESUME FAILED: browser handle missing from registry! \
                 browser_id={:?}, url={}. The browser was destroyed while suspended.",
                browser_id,
                url,
            );
        }

        self.url = url;
        // Page is still loaded — just un-hide and un-mute
        self.set_hidden(false);
        self.set_audio_muted(false);
    }

    pub fn reload(&mut self) {
        if self.with_browser(|browser| browser.reload()).is_some() {
            self.is_loading = true;
        } else {
            log::trace!(
                "[browser] reload called but no browser exists, url={}",
                self.url,
            );
        }
    }

    pub fn stop(&mut self) {
        self.with_browser(|browser| browser.stop_load());
        self.is_loading = false;
    }

    pub fn go_back(&mut self) {
        if self.can_go_back {
            self.with_browser(|browser| browser.go_back());
        }
    }

    pub fn go_forward(&mut self) {
        if self.can_go_forward {
            self.with_browser(|browser| browser.go_forward());
        }
    }

    pub fn copy(&self) {
        self.with_focused_frame(|frame| frame.copy());
    }

    pub fn cut(&self) {
        self.with_focused_frame(|frame| frame.cut());
    }

    pub fn paste(&self) {
        self.with_focused_frame(|frame| frame.paste());
    }

    pub fn undo(&self) {
        self.with_focused_frame(|frame| frame.undo());
    }

    pub fn redo(&self) {
        self.with_focused_frame(|frame| frame.redo());
    }

    pub fn select_all(&self) {
        self.with_focused_frame(|frame| frame.select_all());
    }

    pub fn delete(&self) {
        self.with_focused_frame(|frame| frame.del());
    }

    pub fn execute_javascript(&self, code: &str) {
        self.with_focused_frame(|frame| {
            let code = cef::CefString::from(code);
            let url = cef::CefString::from("");
            frame.execute_java_script(Some(&code), Some(&url), 0);
        });
    }

    pub fn open_devtools(&self) {
        self.with_host(|host| {
            let window_info = cef::WindowInfo::default();
            let settings = cef::BrowserSettings::default();
            let point = cef::Point { x: 0, y: 0 };
            host.show_dev_tools(Some(&window_info), None, Some(&settings), Some(&point));
        });
    }

    pub fn set_size(&mut self, width: u32, height: u32) {
        {
            let mut state = self.render_state.lock();
            state.width = width;
            state.height = height;
        }
        self.with_host(|host| {
            host.was_resized();
        });
    }

    pub fn set_scale_factor(&mut self, scale: f32) {
        self.render_state.lock().scale_factor = scale;
    }

    pub fn invalidate(&self) {
        self.with_host(|host| {
            host.invalidate(cef::PaintElementType::default());
        });
    }

    pub fn set_focus(&self, focus: bool) {
        self.with_host(|host| {
            host.set_focus(if focus { 1 } else { 0 });
        });
    }

    pub fn set_hidden(&self, hidden: bool) {
        self.with_host(|host| {
            host.was_hidden(if hidden { 1 } else { 0 });
        });
    }

    pub fn set_audio_muted(&self, muted: bool) {
        self.with_host(|host| {
            host.set_audio_muted(if muted { 1 } else { 0 });
        });
    }

    pub fn find_in_page(&self, query: &str, forward: bool, match_case: bool, find_next: bool) {
        self.with_host(|host| {
            let query = cef::CefString::from(query);
            host.find(
                Some(&query),
                if forward { 1 } else { 0 },
                if match_case { 1 } else { 0 },
                if find_next { 1 } else { 0 },
            );
        });
    }

    pub fn stop_finding(&self, clear_selection: bool) {
        self.with_host(|host| {
            host.stop_finding(if clear_selection { 1 } else { 0 });
        });
    }

    pub fn ime_set_composition(&self, text: &str, selection_range: Option<std::ops::Range<usize>>) {
        self.with_host(|host| {
            let text = cef::CefString::from(text);
            let selection_range = selection_range.map(|range| cef::Range {
                from: range.start as u32,
                to: range.end as u32,
            });
            host.ime_set_composition(Some(&text), None, None, selection_range.as_ref());
        });
    }

    pub fn insert_committed_text(&self, text: &str) {
        for character in text.encode_utf16() {
            self.send_key_event(&KeyEvent {
                type_: KeyEventType::CHAR,
                modifiers: 0,
                windows_key_code: character as i32,
                character,
                unmodified_character: character,
                focus_on_editable_field: 1,
                ..Default::default()
            });
        }
    }

    pub fn ime_finish_composing_text(&self, keep_selection: bool) {
        self.with_host(|host| {
            host.ime_finish_composing_text(if keep_selection { 1 } else { 0 });
        });
    }

    pub fn ime_cancel_composition(&self) {
        self.with_host(|host| {
            host.ime_cancel_composition();
        });
    }

    pub fn send_key_event(&self, event: &cef::KeyEvent) {
        self.with_host(|host| {
            crate::client::MANUAL_KEY_EVENT.store(true, Ordering::Relaxed);
            host.send_key_event(Some(event));
            crate::client::MANUAL_KEY_EVENT.store(false, Ordering::Relaxed);
        });
    }

    pub fn start_download(&self, url: &str) {
        self.with_host(|host| {
            let url = cef::CefString::from(url);
            host.start_download(Some(&url));
        });
    }

    pub fn send_mouse_click(
        &self,
        x: i32,
        y: i32,
        button: MouseButtonType,
        is_down: bool,
        click_count: i32,
        modifiers: u32,
    ) {
        self.with_host(|host| {
            let event = cef::MouseEvent { x, y, modifiers };
            host.send_mouse_click_event(
                Some(&event),
                button,
                if is_down { 0 } else { 1 },
                click_count,
            );
        });
    }

    pub fn send_mouse_move(&self, x: i32, y: i32, mouse_leave: bool, modifiers: u32) {
        self.with_host(|host| {
            let event = cef::MouseEvent { x, y, modifiers };
            host.send_mouse_move_event(Some(&event), if mouse_leave { 1 } else { 0 });
        });
    }

    pub fn send_mouse_wheel(&self, x: i32, y: i32, delta_x: i32, delta_y: i32, modifiers: u32) {
        self.with_host(|host| {
            let event = cef::MouseEvent { x, y, modifiers };
            host.send_mouse_wheel_event(Some(&event), delta_x, delta_y);
        });
    }

    #[cfg(target_os = "macos")]
    pub fn current_frame(&self) -> Option<CVPixelBuffer> {
        self.current_frame.lock().clone()
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn is_loading(&self) -> bool {
        self.is_loading
    }

    pub fn can_go_back(&self) -> bool {
        self.can_go_back
    }

    pub fn can_go_forward(&self) -> bool {
        self.can_go_forward
    }

    pub fn favicon_url(&self) -> Option<&str> {
        self.favicon_url.as_deref()
    }

    pub fn page_chrome_color(&self) -> Option<Hsla> {
        self.page_chrome.map(|page_chrome| page_chrome.color)
    }

    pub(crate) fn text_input_state(&self) -> BrowserTextInputState {
        self.text_input_state
    }

    pub fn is_new_tab_page(&self) -> bool {
        self.is_new_tab_page
    }

    pub fn set_new_tab_page(&mut self, value: bool) {
        self.is_new_tab_page = value;
        if value {
            self.page_chrome = None;
        }
    }

    pub fn set_pending_url(&mut self, url: String) {
        self.url = url.clone();
        self.title = url.clone();
        self.pending_url = Some(url);
    }

    pub fn take_pending_url(&mut self) -> Option<String> {
        self.pending_url.take()
    }

    pub fn has_pending_url(&self) -> bool {
        self.pending_url.is_some()
    }

    pub fn is_pinned(&self) -> bool {
        self.is_pinned
    }

    pub fn set_pinned(&mut self, value: bool) {
        self.is_pinned = value;
    }

    pub fn set_request_context(&mut self, request_context: Option<cef::RequestContext>) {
        self.request_context = request_context;
    }

    pub fn close_browser(&mut self) {
        self.page_chrome = None;
        if let Some(browser_id) = self.browser_id.take() {
            let browser = BROWSER_HANDLES
                .lock()
                .as_mut()
                .and_then(|m| m.remove(&browser_id));
            if let Some(browser) = browser {
                let is_valid = browser.is_valid();
                if let Some(host) = browser.host() {
                    let ready_to_close = host.is_ready_to_be_closed();
                    log::trace!(
                        "[browser::tab] close_browser: id={} url={} is_valid={} ready_to_close={}",
                        browser_id,
                        self.url,
                        is_valid,
                        ready_to_close,
                    );
                    host.close_browser(1);
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            *self.current_frame.lock() = None;
        }
    }

    /// Access the CEF browser handle from the global registry.
    /// Returns None if the browser was never created or was already
    /// taken by shutdown / close_browser.
    fn with_browser<R>(&self, callback: impl FnOnce(&cef::Browser) -> R) -> Option<R> {
        let browser_id = self.browser_id?;
        let handles = BROWSER_HANDLES.lock();
        handles.as_ref()?.get(&browser_id).map(callback)
    }

    fn with_host(&self, callback: impl FnOnce(&cef::BrowserHost)) {
        self.with_browser(|browser| {
            if let Some(host) = browser.host() {
                callback(&host);
            }
        });
    }

    fn with_focused_frame(&self, callback: impl FnOnce(&cef::Frame)) {
        self.with_browser(|browser| {
            if let Some(frame) = browser.focused_frame() {
                callback(&frame);
            }
        });
    }
}

impl Drop for BrowserTab {
    fn drop(&mut self) {
        if let Some(browser_id) = self.browser_id.take() {
            // If the handle is still in the registry, this is a normal drop
            // (not during CEF shutdown). Take it out and close it.
            // If shutdown already took it via close_all_browsers(), the
            // remove returns None and we skip all CEF API calls.
            let browser = BROWSER_HANDLES
                .lock()
                .as_mut()
                .and_then(|m| m.remove(&browser_id));
            if let Some(browser) = browser {
                log::trace!(
                    "[browser::tab] Drop: id={} url={} is_valid={}",
                    browser_id,
                    self.url,
                    browser.is_valid(),
                );
                if let Some(host) = browser.host() {
                    log::trace!(
                        "[browser::tab] Drop: id={} ready_to_close={}, calling close_browser(1)",
                        browser_id,
                        host.is_ready_to_be_closed(),
                    );
                    host.close_browser(1);
                }
            }
        }
    }
}
