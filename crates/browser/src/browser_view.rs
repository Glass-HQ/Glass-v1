mod actions;
mod bookmarks;
mod content;
mod context_menu;
mod input;
mod navigation;
mod session;
mod swipe;
mod tab_strip;
mod tabs;

pub use tab_strip::BrowserSidebarPanel;

use self::context_menu::{BrowserContextMenu, PendingContextMenu};
use self::swipe::SwipeNavigationState;

use crate::bookmarks::BookmarkBar;
use crate::cef_instance::CefInstance;
use crate::events::{BrowserTabOpenTarget, DownloadUpdatedEvent, OpenTargetRequest};
use crate::history::BrowserHistory;
use crate::session::{SerializedDownloadItem, SerializedTab};
use crate::tab::{BrowserTab, TabEvent};
use crate::text_input::BrowserTextInputState;
#[cfg(not(target_os = "macos"))]
use crate::toolbar::{BrowserToolbar, BrowserToolbarStyle};
use editor::{Editor, actions::SelectAll as EditorSelectAll};
use gpui::px;
use gpui::{
    App, Bounds, Context, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, SharedString, Styled,
    Subscription, Task, UTF16Selection, WeakEntity, Window, actions, div, point, prelude::*, size,
};
#[cfg(not(target_os = "macos"))]
use gpui::UniformListScrollHandle;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use workspace::{
    Workspace,
    item::{Item, ItemEvent, TabTooltipContent, WorkspaceItemKind},
};
use workspace_modes::ModeNavigationEntry;

const MAX_CLOSED_TABS: usize = 20;

static TABS_RESTORED: AtomicBool = AtomicBool::new(false);

actions!(
    browser,
    [
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        SelectAll,
        NewTab,
        CloseTab,
        ReopenClosedTab,
        NextTab,
        PreviousTab,
        FocusOmnibox,
        Reload,
        GoBack,
        GoForward,
        OpenDevTools,
        PinTab,
        UnpinTab,
        OpenBrowserPane,
        BookmarkCurrentPage,
        CopyUrl,
        ToggleSidebar,
        FindInPage,
        FindNextInPage,
        FindPreviousInPage,
        CloseFindInPage,
        ToggleDownloadCenter,
    ]
);

#[derive(Default, Debug, Clone, Copy, PartialEq)]
enum TabBarMode {
    #[default]
    Horizontal,
    Sidebar,
}

#[derive(Clone)]
struct DownloadItemState {
    item: DownloadUpdatedEvent,
    is_incognito: bool,
}

impl DownloadItemState {
    fn from_update(update: &DownloadUpdatedEvent, is_incognito: bool) -> Self {
        Self {
            item: update.clone(),
            is_incognito,
        }
    }

    fn from_serialized(item: SerializedDownloadItem) -> Self {
        Self {
            item: DownloadUpdatedEvent {
                id: item.id,
                url: item.url,
                original_url: item.original_url,
                suggested_file_name: item.suggested_file_name,
                full_path: item.full_path,
                current_speed: item.current_speed,
                percent_complete: item.percent_complete,
                total_bytes: item.total_bytes,
                received_bytes: item.received_bytes,
                is_in_progress: item.is_in_progress,
                is_complete: item.is_complete,
                is_canceled: item.is_canceled,
                is_interrupted: item.is_interrupted,
            },
            is_incognito: false,
        }
    }

    fn update(&mut self, update: &DownloadUpdatedEvent) {
        self.item = update.clone();
    }

    fn to_serialized(&self) -> SerializedDownloadItem {
        SerializedDownloadItem {
            id: self.item.id,
            url: self.item.url.clone(),
            original_url: self.item.original_url.clone(),
            suggested_file_name: self.item.suggested_file_name.clone(),
            full_path: self.item.full_path.clone(),
            current_speed: self.item.current_speed,
            percent_complete: self.item.percent_complete,
            total_bytes: self.item.total_bytes,
            received_bytes: self.item.received_bytes,
            is_in_progress: self.item.is_in_progress,
            is_complete: self.item.is_complete,
            is_canceled: self.item.is_canceled,
            is_interrupted: self.item.is_interrupted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserDownloadItem {
    pub id: u32,
    pub display_name: String,
    pub status_text: String,
    pub full_path: Option<String>,
    pub is_complete: bool,
    pub is_incognito: bool,
}

#[derive(Clone)]
struct PendingTabOpenRequest {
    url: String,
    target: BrowserTabOpenTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserSurfaceState {
    Visible,
    HiddenWarm,
    Suspended,
}

pub struct BrowserView {
    focus_handle: FocusHandle,
    tabs: Vec<Entity<BrowserTab>>,
    active_tab_index: usize,
    closed_tabs: Vec<SerializedTab>,
    #[cfg(not(target_os = "macos"))]
    toolbar: Option<Entity<BrowserToolbar>>,
    bookmark_bar: Entity<BookmarkBar>,
    history: Entity<BrowserHistory>,
    content_bounds: Bounds<Pixels>,
    cef_available: bool,
    is_tab_owner: bool,
    message_pump_started: bool,
    last_viewport: Option<(u32, u32, u32)>,
    pending_tab_opens: Vec<PendingTabOpenRequest>,
    pending_toolbar_sync: bool,
    new_tab_search_text: String,
    new_tab_suggestions: Vec<crate::history::HistoryMatch>,
    new_tab_selected_index: Option<usize>,
    new_tab_search_editor: Option<Entity<Editor>>,
    suppress_new_tab_search_editor_event: bool,
    pending_new_tab_focus: bool,
    context_menu: Option<BrowserContextMenu>,
    pending_context_menu: Option<PendingContextMenu>,
    is_incognito_window: bool,
    incognito_request_context: Option<cef::RequestContext>,
    find_visible: bool,
    find_editor: Option<Entity<Editor>>,
    suppress_find_editor_event: bool,
    find_query: String,
    find_match_count: i32,
    find_active_match_ordinal: i32,
    ime_marked_text: Option<String>,
    ime_selected_range: Option<Range<usize>>,
    download_center_visible: bool,
    downloads: Vec<DownloadItemState>,
    tab_bar_mode: TabBarMode,
    hovered_top_tab_index: Option<usize>,
    hovered_top_tab_close_index: Option<usize>,
    #[cfg(not(target_os = "macos"))]
    hovered_top_new_tab_button: bool,
    #[cfg(not(target_os = "macos"))]
    hovered_sidebar_tab_index: Option<usize>,
    #[cfg(not(target_os = "macos"))]
    hovered_sidebar_tab_close_index: Option<usize>,
    #[cfg(not(target_os = "macos"))]
    hovered_sidebar_new_tab_button: bool,
    #[cfg(not(target_os = "macos"))]
    sidebar_collapsed: bool,
    sidebar_visible: bool,
    #[cfg(not(target_os = "macos"))]
    sidebar_scroll_handle: UniformListScrollHandle,
    native_sidebar_panel: Option<Entity<tab_strip::BrowserSidebarPanel>>,
    focus_listeners_registered: bool,
    toast_layer: Entity<toast::ToastLayer>,
    surface_state: BrowserSurfaceState,
    swipe_state: SwipeNavigationState,
    _swipe_dismiss_task: Option<Task<()>>,
    _message_pump_task: Option<Task<()>>,
    _schedule_save: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl BrowserView {
    fn clear_ime_state(&mut self) {
        self.ime_marked_text = None;
        self.ime_selected_range = None;
    }

    fn set_active_tab_index(&mut self, index: usize) {
        self.active_tab_index = index;
        self.clear_ime_state();
    }

    pub(crate) fn active_tab_text_input_state(&self, cx: &App) -> BrowserTextInputState {
        self.active_tab()
            .map(|tab| tab.read(cx).text_input_state())
            .unwrap_or_default()
    }

    pub(crate) fn text_input_enabled(&self, cx: &App) -> bool {
        self.active_tab_text_input_state(cx)
            .is_active(self.ime_marked_text.is_some())
    }

    pub(crate) fn text_input_composing(&self) -> bool {
        self.ime_marked_text.is_some()
    }

    pub fn new(cx: &mut Context<Self>) -> Self {
        let cef_available = CefInstance::global().is_some();

        let quit_subscription = cx.on_app_quit(Self::save_tabs_on_quit);
        let history = cx.new(|cx| BrowserHistory::new(cx));
        let bookmark_bar = cx.new(|cx| BookmarkBar::new(cx));
        let bookmark_subscription = cx.subscribe(&bookmark_bar, Self::handle_bookmark_bar_event);
        let toast_layer = cx.new(|_| toast::ToastLayer::new());

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            tabs: Vec::new(),
            active_tab_index: 0,
            closed_tabs: Vec::new(),
            #[cfg(not(target_os = "macos"))]
            toolbar: None,
            bookmark_bar,
            history,
            content_bounds: Bounds::default(),
            cef_available,
            is_tab_owner: false,
            message_pump_started: false,
            last_viewport: None,
            pending_tab_opens: Vec::new(),
            pending_toolbar_sync: false,
            new_tab_search_text: String::new(),
            new_tab_suggestions: Vec::new(),
            new_tab_selected_index: None,
            new_tab_search_editor: None,
            suppress_new_tab_search_editor_event: false,
            pending_new_tab_focus: false,
            context_menu: None,
            pending_context_menu: None,
            is_incognito_window: false,
            incognito_request_context: None,
            find_visible: false,
            find_editor: None,
            suppress_find_editor_event: false,
            find_query: String::new(),
            find_match_count: 0,
            find_active_match_ordinal: 0,
            ime_marked_text: None,
            ime_selected_range: None,
            download_center_visible: false,
            downloads: Vec::new(),
            tab_bar_mode: TabBarMode::default(),
            hovered_top_tab_index: None,
            hovered_top_tab_close_index: None,
            #[cfg(not(target_os = "macos"))]
            hovered_top_new_tab_button: false,
            #[cfg(not(target_os = "macos"))]
            hovered_sidebar_tab_index: None,
            #[cfg(not(target_os = "macos"))]
            hovered_sidebar_tab_close_index: None,
            #[cfg(not(target_os = "macos"))]
            hovered_sidebar_new_tab_button: false,
            #[cfg(not(target_os = "macos"))]
            sidebar_collapsed: false,
            sidebar_visible: false,
            #[cfg(not(target_os = "macos"))]
            sidebar_scroll_handle: UniformListScrollHandle::default(),
            native_sidebar_panel: None,
            focus_listeners_registered: false,
            toast_layer,
            surface_state: BrowserSurfaceState::Visible,
            swipe_state: SwipeNavigationState::default(),
            _swipe_dismiss_task: None,
            _message_pump_task: None,
            _schedule_save: None,
            _subscriptions: vec![quit_subscription, bookmark_subscription],
        };

        if cef_available {
            if !this.is_incognito_window {
                this.restore_downloads();
            }
            let already_restored = TABS_RESTORED.swap(true, Ordering::SeqCst);
            this.is_tab_owner = !already_restored;
            let restored = if !already_restored {
                this.restore_tabs(cx)
            } else {
                this.restore_pinned_tabs(cx)
            };
            if !restored {
                this.add_tab(cx);
            }

            this.sync_bookmark_bar_visibility(cx);
        }

        this
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn set_sidebar_visibility(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.sidebar_visible == visible {
            return;
        }

        self.sidebar_visible = visible;
        cx.refresh_windows();
    }

    /// Tell CEF to release focus on the active tab.
    /// Called when the browser mode is deactivated so that native key events
    /// are no longer intercepted by the CEF browser host.
    pub fn release_cef_focus(&self, cx: &App) {
        if let Some(tab) = self.tabs.get(self.active_tab_index) {
            tab.read(cx).set_focus(false);
        }
    }

    pub fn set_surface_state(
        &mut self,
        state: BrowserSurfaceState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.surface_state == state {
            return;
        }

        self.surface_state = state;
        self.apply_surface_state(window, cx);
    }

    pub fn park_surface(&mut self, cx: &mut Context<Self>) {
        if self.surface_state == BrowserSurfaceState::HiddenWarm {
            return;
        }

        self.surface_state = BrowserSurfaceState::HiddenWarm;
        if let Some(tab) = self.active_tab().cloned() {
            tab.update(cx, |tab, _| {
                tab.set_focus(false);
                tab.set_hidden(true);
            });
        }
        cx.notify();
    }

    fn apply_surface_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab().cloned() else {
            return;
        };

        match self.surface_state {
            BrowserSurfaceState::Visible => {
                let (width, height, scale_factor) = self.current_dimensions(window);
                tab.update(cx, |tab, _| {
                    if tab.is_suspended() {
                        tab.set_scale_factor(scale_factor);
                        tab.set_size(width, height);
                        tab.resume();
                        if !tab.has_browser() && width > 0 && height > 0 {
                            let url = tab.url().to_string();
                            if let Err(error) = tab.create_browser(&url) {
                                log::error!(
                                    "[browser] failed to recreate suspended surface for {}: {}",
                                    url,
                                    error,
                                );
                                return;
                            }
                        }
                    }
                    tab.set_hidden(false);
                });
            }
            BrowserSurfaceState::HiddenWarm => {
                tab.update(cx, |tab, _| {
                    tab.set_focus(false);
                    tab.set_hidden(true);
                });
            }
            BrowserSurfaceState::Suspended => {
                tab.update(cx, |tab, _| {
                    tab.set_focus(false);
                    tab.suspend();
                });
            }
        }

        cx.notify();
    }

    pub fn active_tab(&self) -> Option<&Entity<BrowserTab>> {
        self.tabs.get(self.active_tab_index)
    }

    pub(crate) fn navigation_entries(&self, cx: &App) -> Vec<ModeNavigationEntry> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(index, tab_entity)| {
                let tab = tab_entity.read(cx);
                let title = tab.title();
                ModeNavigationEntry {
                    id: SharedString::from(tab_entity.entity_id().as_u64().to_string()),
                    label: SharedString::from(title.to_string()),
                    detail: None,
                    is_pinned: tab.is_pinned(),
                    is_selected: index == self.active_tab_index,
                }
            })
            .collect()
    }

    pub(crate) fn activate_navigation_entry(
        &mut self,
        tab_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.entity_id().as_u64() == tab_id)
        else {
            return;
        };
        self.switch_to_tab(index, window, cx);
    }

    pub(crate) fn close_navigation_entry(
        &mut self,
        tab_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.entity_id().as_u64() == tab_id)
        else {
            return;
        };
        self.close_tab_at(index, window, cx);
    }

    pub fn history(&self) -> &Entity<BrowserHistory> {
        &self.history
    }

    pub(crate) fn new_tab_search_text(&self) -> &str {
        &self.new_tab_search_text
    }

    // Failure modes:
    // - The new tab editor is first needed during a focus request before it has rendered.
    // - Blur can fire while the user clicks a suggestion row.
    // - Async history search results can arrive after the query has already changed.
    fn ensure_new_tab_search_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Editor> {
        if let Some(editor) = self.new_tab_search_editor.clone() {
            return editor;
        }

        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search or enter URL", window, cx);
            editor
        });
        let editor_subscription = cx.subscribe(&editor, Self::handle_new_tab_search_editor_event);
        self._subscriptions.push(editor_subscription);
        self.new_tab_search_editor = Some(editor.clone());
        editor
    }

    fn handle_new_tab_search_editor_event(
        &mut self,
        editor: Entity<Editor>,
        event: &editor::EditorEvent,
        cx: &mut Context<Self>,
    ) {
        if self.suppress_new_tab_search_editor_event
            || !matches!(event, editor::EditorEvent::BufferEdited)
        {
            return;
        }

        self.set_new_tab_search_text(editor.read(cx).text(cx), cx);
    }

    pub(crate) fn new_tab_search_editor_entity(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Editor> {
        self.ensure_new_tab_search_editor(window, cx)
    }

    fn new_tab_search_editor_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.new_tab_search_editor
            .as_ref()
            .is_some_and(|editor| editor.focus_handle(cx).contains_focused(window, cx))
    }

    pub(crate) fn focus_new_tab_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.ensure_new_tab_search_editor(window, cx);
        window.focus(&editor.focus_handle(cx), cx);
        editor.update(cx, |editor, cx| {
            editor.select_all(&EditorSelectAll, window, cx);
        });
    }

    fn defer_focus_new_tab_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.ensure_new_tab_search_editor(window, cx);
        cx.defer_in(window, move |_this, window, cx| {
            window.focus(&editor.focus_handle(cx), cx);
            editor.update(cx, |editor, cx| {
                editor.select_all(&EditorSelectAll, window, cx);
            });
        });
    }

    pub(crate) fn set_new_tab_search_text(&mut self, text: String, cx: &mut Context<Self>) {
        if self.new_tab_search_text == text {
            return;
        }

        self.new_tab_search_text = text.clone();
        self.new_tab_selected_index = None;
        self.new_tab_suggestions.clear();

        if text.is_empty() {
        } else {
            self.search_new_tab_history(text, cx);
        }
        cx.notify();
    }

    fn new_tab_row_count(&self) -> usize {
        let mut count = self.new_tab_suggestions.len();
        if !self.new_tab_search_text.is_empty() {
            count += 1; // "Search Google" row
        }
        count
    }

    fn clear_new_tab_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_tab_search_text.clear();
        self.new_tab_suggestions.clear();
        self.new_tab_selected_index = None;

        if let Some(editor) = self.new_tab_search_editor.clone() {
            self.suppress_new_tab_search_editor_event = true;
            editor.update(cx, |editor, cx| {
                editor.set_text(String::new(), window, cx);
            });
            self.suppress_new_tab_search_editor_event = false;
        }
    }

    fn navigate_new_tab_search_result(
        &mut self,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_new_tab_search(window, cx);

        if let Some(tab) = self.active_tab().cloned() {
            tab.update(cx, |tab, cx| {
                tab.navigate(&url, cx);
                tab.set_focus(true);
            });
        }

        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    pub(crate) fn submit_new_tab_search(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let url = if let Some(index) = self.new_tab_selected_index {
            self.url_for_new_tab_row(index)
                .unwrap_or_else(|| text_to_url(text.trim()))
        } else {
            let query = text.trim();
            if query.is_empty() {
                return;
            }
            text_to_url(query)
        };

        self.navigate_new_tab_search_result(url, window, cx);
    }

    pub(crate) fn activate_new_tab_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(url) = self.url_for_new_tab_row(index) else {
            return;
        };

        self.navigate_new_tab_search_result(url, window, cx);
    }

    pub(crate) fn new_tab_move_up(&mut self, cx: &mut Context<Self>) {
        let total = self.new_tab_row_count();
        if total == 0 {
            return;
        }
        self.new_tab_selected_index = Some(match self.new_tab_selected_index {
            Some(0) | None => total.saturating_sub(1),
            Some(i) => i - 1,
        });
        cx.notify();
    }

    pub(crate) fn new_tab_move_down(&mut self, cx: &mut Context<Self>) {
        let total = self.new_tab_row_count();
        if total == 0 {
            return;
        }
        self.new_tab_selected_index = Some(match self.new_tab_selected_index {
            Some(i) => (i + 1) % total,
            None => 0,
        });
        cx.notify();
    }

    pub(crate) fn new_tab_cancel(&mut self, cx: &mut Context<Self>) {
        self.new_tab_suggestions.clear();
        self.new_tab_selected_index = None;
        cx.notify();
    }

    fn search_new_tab_history(&mut self, query: String, cx: &mut Context<Self>) {
        let entries = self.history.read(cx).entries().to_vec();
        let executor = cx.background_executor().clone();
        let requested_query = query.clone();
        cx.spawn(async move |this, cx| {
            let matches = crate::history::BrowserHistory::search(entries, query, 8, executor).await;
            let _ = cx.update(|cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.new_tab_search_text != requested_query {
                        return;
                    }
                    this.new_tab_suggestions = matches;
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn url_for_new_tab_row(&self, index: usize) -> Option<String> {
        let mut current = 0;

        if !self.new_tab_search_text.is_empty() {
            if current == index {
                return Some(text_to_url(&self.new_tab_search_text));
            }
            current += 1;
        }

        let suggestion_index = index - current;
        self.new_tab_suggestions
            .get(suggestion_index)
            .map(|suggestion| suggestion.url.clone())
    }

    fn request_context_for_new_tab(&self) -> Option<cef::RequestContext> {
        if self.is_incognito_window {
            self.incognito_request_context.clone()
        } else {
            None
        }
    }

    fn configure_tab_request_context(&self, tab: &Entity<BrowserTab>, cx: &mut Context<Self>) {
        let request_context = self.request_context_for_new_tab();
        tab.update(cx, |tab, _| {
            tab.set_request_context(request_context);
        });
    }

    fn ensure_incognito_request_context(&mut self) {
        if self.incognito_request_context.is_some() {
            return;
        }

        let settings = cef::RequestContextSettings::default();
        self.incognito_request_context = cef::request_context_create_context(Some(&settings), None);
        if self.incognito_request_context.is_none() {
            log::error!("[browser] failed to create incognito request context");
        }
    }

    pub fn configure_as_incognito_window(&mut self, cx: &mut Context<Self>) {
        if self.is_incognito_window {
            return;
        }

        log::info!("[browser] switching browser view into incognito mode");
        self.is_incognito_window = true;
        self.ensure_incognito_request_context();

        for tab in &self.tabs {
            tab.update(cx, |tab, _| {
                tab.stop_finding(true);
                tab.close_browser();
            });
        }

        self.tabs.clear();
        self.closed_tabs.clear();
        self.set_active_tab_index(0);
        self.pending_tab_opens.clear();
        self.pending_toolbar_sync = true;
        self.context_menu = None;
        self.pending_context_menu = None;
        self.find_visible = false;
        self.find_query.clear();
        self.find_match_count = 0;
        self.find_active_match_ordinal = 0;
        self.download_center_visible = false;
        self.downloads.clear();
        self._schedule_save = None;

        self.history.update(cx, |history, _| {
            history.clear();
        });

        self.add_tab(cx);
        self.sync_bookmark_bar_visibility(cx);
        cx.notify();
    }

    pub(crate) fn update_toolbar_active_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = window;

        #[cfg(not(target_os = "macos"))]
        if let (Some(toolbar), Some(tab)) = (self.toolbar.clone(), self.active_tab().cloned()) {
            toolbar.update(cx, |toolbar, cx| {
                toolbar.set_active_tab(tab, window, cx);
            });
        }

        self.sync_bookmark_bar_visibility(cx);
    }

    #[cfg(not(target_os = "macos"))]
    fn focus_new_tab_search_if_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let is_new_tab = self
            .active_tab()
            .map(|t| t.read(cx).is_new_tab_page())
            .unwrap_or(false);
        if !is_new_tab {
            return;
        }

        self.focus_new_tab_search(window, cx);
    }

    fn request_new_tab_search_focus(&mut self, cx: &mut Context<Self>) {
        let is_new_tab = self
            .active_tab()
            .map(|t| t.read(cx).is_new_tab_page())
            .unwrap_or(false);
        if is_new_tab {
            self.pending_new_tab_focus = true;
        }
    }

    fn sync_bookmark_bar_visibility(&self, cx: &mut Context<Self>) {
        let is_new_tab_page = self
            .active_tab()
            .map(|t| t.read(cx).is_new_tab_page())
            .unwrap_or(true);
        self.bookmark_bar.update(cx, |bar, _| {
            bar.set_active_tab_is_new_tab_page(is_new_tab_page);
        });
    }

    fn handle_tab_event(
        &mut self,
        tab_entity: Entity<BrowserTab>,
        event: &TabEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            #[cfg(target_os = "macos")]
            TabEvent::FrameReady => {
                if self.surface_state == BrowserSurfaceState::Visible {
                    cx.notify();
                }
            }
            TabEvent::NavigateToUrl(url) => {
                let url = url.clone();
                self.create_browser_and_navigate(&tab_entity, &url, cx);
            }
            TabEvent::OpenNewTab(url) => {
                self.queue_tab_open(url.clone(), BrowserTabOpenTarget::Background, cx);
            }
            TabEvent::OpenTargetRequested(request) => {
                self.handle_open_target_request(tab_entity, request, cx);
            }
            TabEvent::AddressChanged(_) | TabEvent::TitleChanged => {
                if !self.is_incognito_window {
                    let tab_handle = tab_entity;
                    let history = self.history.clone();
                    cx.defer(move |cx| {
                        let (url, title) = {
                            let tab = tab_handle.read(cx);
                            (tab.url().to_string(), tab.title().to_string())
                        };
                        history.update(cx, |history, _| {
                            history.record_visit(&url, &title);
                        });
                    });
                    self.schedule_save(cx);
                }
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }
            TabEvent::FaviconChanged => {
                self.schedule_save(cx);
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }
            TabEvent::LoadingStateChanged => {
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }
            TabEvent::PageChromeChanged => {
                let is_active_tab = self
                    .active_tab()
                    .is_some_and(|active_tab| active_tab == &tab_entity);
                if is_active_tab {
                    cx.notify();
                }
            }
            TabEvent::TextInputStateChanged(text_input_state) => {
                let is_active_tab = self
                    .active_tab()
                    .is_some_and(|active_tab| active_tab == &tab_entity);
                if is_active_tab {
                    if !text_input_state.editable {
                        self.clear_ime_state();
                    }
                    cx.notify();
                }
            }
            TabEvent::LoadError { url, error_text } => {
                log::warn!("[browser] load error: url={} err={}", url, error_text);
                cx.notify();
            }
            TabEvent::ContextMenuOpen { context } => {
                self.pending_context_menu = Some(PendingContextMenu {
                    context: context.clone(),
                });
                cx.notify();
            }
            TabEvent::FindResult(result) => {
                let is_active_tab = self
                    .active_tab()
                    .is_some_and(|active_tab| active_tab == &tab_entity);
                if is_active_tab {
                    self.find_match_count = result.count;
                    self.find_active_match_ordinal = result.active_match_ordinal;
                    cx.notify();
                }
            }
            TabEvent::DownloadUpdated(update) => {
                self.update_download(update, cx);
                cx.notify();
            }
        }
    }

    fn handle_open_target_request(
        &mut self,
        _tab_entity: Entity<BrowserTab>,
        request: &OpenTargetRequest,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = request.disposition.app_tab_target() else {
            return;
        };

        self.queue_tab_open(request.url.clone(), target, cx);
    }

    fn queue_tab_open(
        &mut self,
        url: String,
        target: BrowserTabOpenTarget,
        cx: &mut Context<Self>,
    ) {
        self.pending_tab_opens
            .push(PendingTabOpenRequest { url, target });
        if matches!(target, BrowserTabOpenTarget::Foreground) {
            self.pending_toolbar_sync = true;
        }
        cx.notify();
    }

    fn update_download(&mut self, update: &DownloadUpdatedEvent, cx: &mut Context<Self>) {
        if let Some(existing) = self
            .downloads
            .iter_mut()
            .find(|item| item.item.id == update.id)
        {
            existing.update(update);
        } else {
            self.downloads.insert(
                0,
                DownloadItemState::from_update(update, self.is_incognito_window),
            );
        }

        if !self.is_incognito_window {
            self.schedule_save(cx);
        }
    }

    pub fn download_items(&self) -> Vec<BrowserDownloadItem> {
        self.downloads
            .iter()
            .map(|download| BrowserDownloadItem {
                id: download.item.id,
                display_name: Self::download_display_name(download),
                status_text: Self::download_status_line(download),
                full_path: download.item.full_path.clone(),
                is_complete: download.item.is_complete,
                is_incognito: download.is_incognito,
            })
            .collect()
    }

    pub fn is_incognito_window(&self) -> bool {
        self.is_incognito_window
    }

    pub(crate) fn toggle_download_center(&mut self, cx: &mut Context<Self>) {
        self.download_center_visible = !self.download_center_visible;
        cx.notify();
    }

    #[cfg(not(target_os = "macos"))]
    fn create_toolbar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().is_some() {
            let browser_view = cx.entity().downgrade();
            let history = self.history.clone();
            let browser_focus_handle = self.focus_handle.clone();
            let active_tab = self.active_tab().cloned();
            let toolbar = cx.new(|cx| {
                BrowserToolbar::new(
                    browser_view,
                    history,
                    browser_focus_handle,
                    active_tab,
                    BrowserToolbarStyle::TitleBar,
                    window,
                    cx,
                )
            });
            self.toolbar = Some(toolbar.clone());

            ModeViewRegistry::global_mut(cx)
                .set_titlebar_center_view(ModeId::BROWSER, toolbar.into());
            self.focus_new_tab_search_if_new_tab(window, cx);
            cx.notify();
        }
    }
}

fn text_to_url(text: &str) -> String {
    if text.starts_with("http://") || text.starts_with("https://") {
        return text.to_string();
    }

    if !looks_like_url(text) {
        let encoded: String = url::form_urlencoded::byte_serialize(text.as_bytes()).collect();
        return format!("https://www.google.com/search?q={}", encoded);
    }

    if should_use_http_by_default(text) {
        format!("http://{text}")
    } else {
        format!("https://{text}")
    }
}

fn looks_like_url(input: &str) -> bool {
    if input.starts_with("http://") || input.starts_with("https://") {
        return true;
    }

    if input.contains("://") {
        return true;
    }

    if input.chars().any(char::is_whitespace) {
        return false;
    }

    let Ok(url) = url::Url::parse(&format!("http://{input}")) else {
        return false;
    };

    let Some(host) = url.host_str() else {
        return false;
    };

    host.eq_ignore_ascii_case("localhost")
        || host.contains('.')
        || host.parse::<std::net::IpAddr>().is_ok()
        || (url.port().is_some() && !host.contains('.'))
}

fn should_use_http_by_default(input: &str) -> bool {
    let Ok(url) = url::Url::parse(&format!("http://{input}")) else {
        return false;
    };

    let Some(host) = url.host_str() else {
        return false;
    };

    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    if let Ok(address) = host.parse::<std::net::IpAddr>() {
        return address.is_loopback();
    }

    url.port().is_some() && !host.contains('.')
}

impl EventEmitter<()> for BrowserView {}
impl EventEmitter<ItemEvent> for BrowserView {}

impl Focusable for BrowserView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for BrowserView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.ime_marked_text.as_ref()?;
        let utf16_len = text.encode_utf16().count();
        if range.end > utf16_len {
            adjusted_range.replace(0..utf16_len);
        }
        Some(text.clone())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.ime_selected_range.clone().unwrap_or(0..0),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.ime_marked_text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let had_marked_text = self.ime_marked_text.is_some();
        self.clear_ime_state();

        if had_marked_text && let Some(tab) = self.active_tab().cloned() {
            tab.update(cx, |tab, _| {
                tab.ime_cancel_composition();
            });
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_ime_state();

        if !text.is_empty()
            && let Some(tab) = self.active_tab().cloned()
        {
            let text = text.to_string();
            tab.update(cx, |tab, _| {
                crate::input::handle_text_commit(tab, &text);
            });
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime_marked_text = Some(new_text.to_string());
        self.ime_selected_range = new_selected_range.clone();

        if let Some(tab) = self.active_tab().cloned() {
            let text = new_text.to_string();
            tab.update(cx, |tab, _| {
                tab.ime_set_composition(&text, new_selected_range.clone());
            });
        }
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(Bounds {
            origin: element_bounds.origin + point(px(8.0), px(8.0)),
            size: size(px(1.0), px(20.0)),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(0)
    }

    fn accepts_text_input(&self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.text_input_enabled(cx)
    }
}

pub struct BrowserPaneItem {
    browser_view: WeakEntity<BrowserView>,
    focus_handle: FocusHandle,
    _workspace: WeakEntity<Workspace>,
}

impl BrowserPaneItem {
    pub fn new(
        browser_view: &Entity<BrowserView>,
        workspace: WeakEntity<Workspace>,
        cx: &mut App,
    ) -> Entity<Self> {
        let focus_handle = browser_view.focus_handle(cx);
        let browser_view = browser_view.downgrade();
        cx.new(|_| Self {
            browser_view,
            focus_handle,
            _workspace: workspace,
        })
    }
}

impl EventEmitter<()> for BrowserPaneItem {}

impl Focusable for BrowserPaneItem {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for BrowserPaneItem {
    type Event = ();

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        SharedString::from("Browser")
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<ui::Icon> {
        Some(ui::Icon::new(ui::IconName::Globe))
    }

    fn workspace_item_kind(&self, _cx: &App) -> Option<WorkspaceItemKind> {
        Some(WorkspaceItemKind::Browser)
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(browser_view) = self.browser_view.upgrade() {
            browser_view.update(cx, |browser_view, cx| {
                browser_view.set_surface_state(BrowserSurfaceState::HiddenWarm, window, cx);
            });
        }
    }

    fn workspace_deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(browser_view) = self.browser_view.upgrade() {
            browser_view.update(cx, |browser_view, cx| {
                browser_view.set_surface_state(BrowserSurfaceState::HiddenWarm, window, cx);
            });
        }
    }
}

impl Render for BrowserPaneItem {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.browser_view
            .upgrade()
            .map(|browser_view| browser_view.into_any_element())
            .unwrap_or_else(|| div().size_full().into_any_element())
    }
}

impl Item for BrowserView {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        let Some(tab) = self.active_tab() else {
            return SharedString::from("Browser");
        };

        let tab = tab.read(cx);
        let title = tab.title().trim();
        if !title.is_empty() && title != "New Tab" {
            SharedString::from(title.to_string())
        } else if tab.url().is_empty() || tab.url() == "glass://newtab" {
            SharedString::from("New Tab")
        } else {
            SharedString::from(tab.url().to_string())
        }
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<ui::Icon> {
        Some(ui::Icon::new(ui::IconName::Globe))
    }

    fn tab_tooltip_content(&self, cx: &App) -> Option<TabTooltipContent> {
        self.active_tab().map(|tab| {
            let url = tab.read(cx).url().to_string();
            TabTooltipContent::Text(SharedString::from(url))
        })
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_surface_state(BrowserSurfaceState::HiddenWarm, window, cx);
    }

    fn workspace_deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_surface_state(BrowserSurfaceState::HiddenWarm, window, cx);
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event);
    }
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.surface_state != BrowserSurfaceState::Visible {
            self.surface_state = BrowserSurfaceState::Visible;
            self.apply_surface_state(window, cx);
        }

        if !self.cef_available {
            return div()
                .id("browser-view")
                .track_focus(&self.focus_handle)
                .size_full()
                .child(self.render_placeholder(cx))
                .into_any_element();
        }

        #[cfg(not(target_os = "macos"))]
        if self.toolbar.is_none() && !self.tabs.is_empty() {
            cx.defer_in(window, |this, window, cx| {
                this.create_toolbar(window, cx);
            });
        }

        #[cfg(not(target_os = "macos"))]
        if let Some(toolbar) = self.toolbar.clone() {
            ModeViewRegistry::global_mut(cx)
                .set_titlebar_center_view(ModeId::BROWSER, toolbar.into());
        }

        if !self.pending_tab_opens.is_empty() {
            self.process_pending_tab_opens(window, cx);
        }

        if !self.focus_listeners_registered {
            self.focus_listeners_registered = true;
            self._subscriptions.push(cx.on_focus_in(
                &self.focus_handle,
                window,
                |this, _window, cx| {
                    if let Some(tab) = this.active_tab().cloned() {
                        tab.update(cx, |tab, _| {
                            tab.set_focus(true);
                        });
                    }
                },
            ));
            self._subscriptions.push(cx.on_focus_out(
                &self.focus_handle,
                window,
                |this, _event, _window, cx| {
                    if let Some(tab) = this.active_tab().cloned() {
                        tab.update(cx, |tab, _| {
                            tab.set_focus(false);
                        });
                    }
                },
            ));
        }

        #[cfg(not(target_os = "macos"))]
        if self.pending_toolbar_sync && self.toolbar.is_some() {
            self.pending_toolbar_sync = false;
            self.update_toolbar_active_tab(window, cx);
        }

        if let Some(pending) = self.pending_context_menu.take() {
            self.open_context_menu(pending.context, window, cx);
        }

        let scale_factor = window.scale_factor();

        let actual_width = f32::from(self.content_bounds.size.width);
        let actual_height = f32::from(self.content_bounds.size.height);
        let has_actual_bounds = actual_width > 0.0 && actual_height > 0.0;

        let (content_width, content_height) = if has_actual_bounds {
            (actual_width as u32, actual_height as u32)
        } else {
            let viewport_size = window.viewport_size();
            (
                f32::from(viewport_size.width) as u32,
                f32::from(viewport_size.height) as u32,
            )
        };

        if content_width > 0 && content_height > 0 {
            if !self.message_pump_started {
                self.ensure_browser_created(content_width, content_height, scale_factor, cx);
                if !self.message_pump_started {
                    cx.notify();
                }
            } else {
                let scale_key = (scale_factor * 1000.0) as u32;
                let new_viewport = (content_width, content_height, scale_key);
                if self.last_viewport != Some(new_viewport) {
                    self.last_viewport = Some(new_viewport);
                    if let Some(tab) = self.active_tab() {
                        tab.update(cx, |tab, _| {
                            tab.set_scale_factor(scale_factor);
                            tab.set_size(content_width, content_height);
                        });
                    }
                }
            }
        }

        if self.focus_handle.is_focused(window) {
            if let Some(tab) = self.active_tab().cloned() {
                tab.update(cx, |tab, _| {
                    tab.set_focus(true);
                });
            }
        }

        if self.pending_new_tab_focus {
            self.pending_new_tab_focus = false;
            self.defer_focus_new_tab_search(window, cx);
        }

        let element = div()
            .id("browser-view")
            .track_focus(&self.focus_handle)
            .key_context("BrowserView")
            .on_key_down(cx.listener(Self::handle_key_down))
            .on_key_up(cx.listener(Self::handle_key_up))
            .on_action(cx.listener(Self::handle_copy))
            .on_action(cx.listener(Self::handle_cut))
            .on_action(cx.listener(Self::handle_paste))
            .on_action(cx.listener(Self::handle_undo))
            .on_action(cx.listener(Self::handle_redo))
            .on_action(cx.listener(Self::handle_select_all))
            .on_action(cx.listener(Self::handle_new_tab))
            .on_action(cx.listener(Self::handle_close_tab))
            .on_action(cx.listener(Self::handle_reopen_closed_tab))
            .on_action(cx.listener(Self::handle_next_tab))
            .on_action(cx.listener(Self::handle_previous_tab))
            .on_action(cx.listener(Self::handle_focus_omnibox))
            .on_action(cx.listener(Self::handle_reload))
            .on_action(cx.listener(Self::handle_go_back))
            .on_action(cx.listener(Self::handle_go_forward))
            .on_action(cx.listener(Self::handle_open_devtools))
            .on_action(cx.listener(Self::handle_bookmark_current_page))
            .on_action(cx.listener(Self::handle_copy_url))
            .on_action(cx.listener(Self::handle_find_in_page))
            .on_action(cx.listener(Self::handle_find_next_in_page))
            .on_action(cx.listener(Self::handle_find_previous_in_page))
            .on_action(cx.listener(Self::handle_close_find_in_page))
            .on_action(cx.listener(Self::handle_toggle_download_center))
            .size_full()
            .flex();

        #[cfg(not(target_os = "macos"))]
        let element = element.on_action(cx.listener(Self::handle_toggle_sidebar));

        #[cfg(target_os = "macos")]
        let element = element
            .flex_col()
            .child(self.bookmark_bar.clone())
            .child(self.render_browser_content(window, cx))
            .into_any_element();

        #[cfg(not(target_os = "macos"))]
        let element = {
            let pinned_count = self.pinned_tab_count(cx);
            match self.tab_bar_mode {
                TabBarMode::Horizontal => element
                    .flex_col()
                    .child(div().mt(px(-1.)).child(self.render_tab_strip(pinned_count, cx)))
                    .child(self.bookmark_bar.clone())
                    .child(self.render_browser_content(window, cx))
                    .into_any_element(),
                TabBarMode::Sidebar => element
                    .flex_row()
                    .child(self.render_sidebar(pinned_count, cx))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .child(self.bookmark_bar.clone())
                            .child(self.render_browser_content(window, cx)),
                    )
                    .into_any_element(),
            }
        };

        div()
            .size_full()
            .relative()
            .child(element)
            .child(self.toast_layer.clone())
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{looks_like_url, text_to_url};

    #[test]
    fn localhost_inputs_are_treated_as_urls() {
        assert!(looks_like_url("localhost"));
        assert!(looks_like_url("localhost:5173"));
        assert_eq!(text_to_url("localhost"), "http://localhost");
        assert_eq!(text_to_url("localhost:5173"), "http://localhost:5173");
    }

    #[test]
    fn regular_domains_default_to_https() {
        assert!(looks_like_url("example.com"));
        assert_eq!(text_to_url("example.com"), "https://example.com");
    }

    #[test]
    fn plain_queries_still_search() {
        assert!(!looks_like_url("rust async await"));
        assert_eq!(
            text_to_url("rust async await"),
            "https://www.google.com/search?q=rust+async+await"
        );
    }
}
