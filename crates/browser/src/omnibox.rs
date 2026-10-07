use crate::history::{BrowserHistory, HistoryMatch};
use editor::{Editor, actions::SelectAll};
use gpui::{
    App, Bounds, Context, Corner, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Pixels, Render, SharedString, Styled, Subscription, Task, Window, anchored,
    canvas, deferred, div, native_image_view, point, prelude::*, px,
};
use std::time::Duration;
use ui::{Icon, IconName, IconSize, h_flex, prelude::*, v_flex};

pub enum OmniboxEvent {
    Navigate(String),
}

pub enum OmniboxSuggestion {
    HistoryItem { url: String, title: String },
    RawUrl(String),
    SearchQuery(String),
}

impl OmniboxSuggestion {
    fn url_or_search(&self) -> String {
        match self {
            OmniboxSuggestion::HistoryItem { url, .. } => url.clone(),
            OmniboxSuggestion::RawUrl(url) => text_to_url(url),
            OmniboxSuggestion::SearchQuery(query) => {
                let encoded: String =
                    url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
                format!("https://www.google.com/search?q={}", encoded)
            }
        }
    }
}

pub struct Omnibox {
    url_editor: Entity<Editor>,
    history: Entity<BrowserHistory>,
    content_focus_handle: FocusHandle,
    suggestions: Vec<OmniboxSuggestion>,
    selected_index: usize,
    is_open: bool,
    suppress_search: bool,
    navigation_started: bool,
    current_page_url: String,
    pending_search: Option<Task<()>>,
    editor_bounds: Bounds<Pixels>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<OmniboxEvent> for Omnibox {}

impl Omnibox {
    pub fn new(
        history: Entity<BrowserHistory>,
        content_focus_handle: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Enter URL or search...", window, cx);
            editor
        });

        let buffer_subscription = cx.subscribe(&url_editor, Self::on_editor_event);
        let focus_subscription =
            cx.on_focus(&url_editor.focus_handle(cx), window, Self::on_editor_focus);
        let blur_subscription =
            cx.on_blur(&url_editor.focus_handle(cx), window, Self::on_editor_blur);

        Self {
            url_editor,
            history,
            content_focus_handle,
            suggestions: Vec::new(),
            selected_index: 0,
            is_open: false,
            suppress_search: false,
            navigation_started: false,
            current_page_url: String::new(),
            pending_search: None,
            editor_bounds: Bounds::default(),
            _subscriptions: vec![buffer_subscription, focus_subscription, blur_subscription],
        }
    }

    pub fn set_url(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let display_url = display_url(url);
        self.navigation_started = false;
        self.current_page_url = display_url.clone();
        self.close_dropdown(cx);
        self.suppress_search = true;
        self.url_editor.update(cx, |editor, cx| {
            editor.set_text(display_url, window, cx);
        });
    }

    #[cfg(not(target_os = "macos"))]
    pub fn focus_and_select_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_dropdown(cx);
        window.focus(&self.url_editor.focus_handle(cx), cx);
        self.url_editor.update(cx, |editor, cx| {
            editor.select_all(&SelectAll, window, cx);
        });
    }

    #[cfg(target_os = "macos")]
    pub fn focus_and_select_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_dropdown(cx);
        window.focus(&self.url_editor.focus_handle(cx), cx);
        self.url_editor.update(cx, |editor, cx| {
            editor.select_all(&SelectAll, window, cx);
        });
    }

    fn on_editor_focus(
        &mut self,
        _: &gpui::FocusEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.suppress_search = true;
        let text = self.url_editor.read(cx).text(cx).to_string();
        if !text.is_empty() {
            self.suppress_search = false;
        }
    }

    fn on_editor_blur(
        &mut self,
        _: &gpui::FocusEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_dropdown(cx);
        if !self.navigation_started {
            let current_page_url = self.current_page_url.clone();
            self.suppress_search = true;
            self.url_editor.update(cx, |editor, cx| {
                // We can't access window here, so we use a no-op window update
                let _ = editor.text(cx);
                let _ = current_page_url;
            });
        }
    }

    fn on_editor_event(
        &mut self,
        _editor: Entity<Editor>,
        event: &editor::EditorEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            editor::EditorEvent::BufferEdited => {
                if self.suppress_search {
                    self.suppress_search = false;
                    return;
                }
                self.schedule_search(cx);
            }
            _ => {}
        }
    }

    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let query = self.url_editor.read(cx).text(cx).to_string();

        if query.is_empty() {
            self.close_dropdown(cx);
            return;
        }

        let executor = cx.background_executor().clone();
        self.pending_search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;

            let query_for_search = this
                .read_with(cx, |this, cx| this.url_editor.read(cx).text(cx).to_string())
                .ok()
                .unwrap_or_default();

            if query_for_search != query {
                return;
            }

            let entries = this
                .read_with(cx, |this, cx| this.history.read(cx).entries().to_vec())
                .ok()
                .unwrap_or_default();

            let history_matches =
                BrowserHistory::search(entries, query_for_search.clone(), 8, executor).await;

            let _ = this.update(cx, |this, cx| {
                this.build_suggestions(query_for_search, history_matches);
                this.pending_search = None;
                cx.notify();
            });
        }));
    }

    fn build_suggestions(&mut self, query: String, history_matches: Vec<HistoryMatch>) {
        self.suggestions.clear();

        // When the input looks like a URL, navigate to it by default (index 0).
        // The search fallback is still available as the second option.
        if looks_like_url(&query) {
            self.suggestions.push(OmniboxSuggestion::RawUrl(query.clone()));
            self.suggestions.push(OmniboxSuggestion::SearchQuery(query));
        } else {
            self.suggestions.push(OmniboxSuggestion::SearchQuery(query));
        }

        for m in history_matches {
            self.suggestions.push(OmniboxSuggestion::HistoryItem {
                url: m.url,
                title: m.title,
            });
        }

        self.selected_index = 0;
        self.is_open = true;
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_open && !self.suggestions.is_empty() {
            let index = self
                .selected_index
                .min(self.suggestions.len().saturating_sub(1));
            let url = self.suggestions[index].url_or_search();
            self.navigate(url, window, cx);
            return;
        }

        // Fallback: if dropdown is not open, just navigate to whatever is in the editor
        let text = self.url_editor.read(cx).text(cx);
        if text.is_empty() {
            return;
        }

        let url = text_to_url(&text);

        self.navigate(url, window, cx);
    }

    fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.close_dropdown(cx);
        self.navigation_started = false;
        let current_page_url = self.current_page_url.clone();
        if self.url_editor.read(cx).text(cx) != current_page_url {
            self.suppress_search = true;
            self.url_editor.update(cx, |editor, cx| {
                editor.set_text(current_page_url, window, cx);
            });
        }
    }

    fn close_dropdown(&mut self, cx: &mut Context<Self>) {
        self.suggestions.clear();
        self.is_open = false;
        self.selected_index = 0;
        self.pending_search = None;
        cx.notify();
    }

    fn navigate(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        self.navigation_started = true;
        self.close_dropdown(cx);
        cx.emit(OmniboxEvent::Navigate(url));
        window.focus(&self.content_focus_handle, cx);
    }

    fn move_up(
        &mut self,
        _: &zed_actions::editor::MoveUp,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_open || self.suggestions.is_empty() {
            return;
        }
        if self.selected_index == 0 {
            self.selected_index = self.suggestions.len() - 1;
        } else {
            self.selected_index -= 1;
        }
        cx.notify();
    }

    fn move_down(
        &mut self,
        _: &zed_actions::editor::MoveDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_open || self.suggestions.is_empty() {
            return;
        }
        self.selected_index = (self.selected_index + 1) % self.suggestions.len();
        cx.notify();
    }
}

impl Focusable for Omnibox {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.url_editor.focus_handle(cx)
    }
}

impl Render for Omnibox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show_dropdown = self.is_open && !self.suggestions.is_empty();

        div()
            .id("omnibox")
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .child(
                canvas(
                    move |bounds, _, cx| cx.with_element_id(Some("omnibox-bounds"), |cx| {
                        cx.defer_draw_order(|_| {}); // just capture bounds
                        let _ = bounds;
                    }),
                    move |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(Icon::new(IconName::MagnifyingGlass).size(IconSize::Small).color(Color::Muted))
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(self.url_editor.clone())
                    )
            )
            .when(show_dropdown, |this| this.child(self.render_dropdown(cx)))
    }
}

impl Omnibox {
    fn render_dropdown(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let suggestions = self.suggestions.iter().enumerate().map(|(index, suggestion)| {
            let is_selected = index == self.selected_index;
            let label: SharedString = match suggestion {
                OmniboxSuggestion::HistoryItem { url, title } => {
                    if title.is_empty() { url.clone().into() } else { title.clone().into() }
                }
                OmniboxSuggestion::RawUrl(url) => url.clone().into(),
                OmniboxSuggestion::SearchQuery(query) => format!("Search: {}", query).into(),
            };
            let icon = match suggestion {
                OmniboxSuggestion::HistoryItem { url, .. } => {
                    Some(native_image_view(url.clone()))
                }
                OmniboxSuggestion::RawUrl(_) => None,
                OmniboxSuggestion::SearchQuery(_) => None,
            };

            div()
                .id(("omnibox-suggestion", index))
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .when(is_selected, |this| this.bg(cx.theme().colors().element_selected))
                .hover(|this| this.bg(cx.theme().colors().element_hover))
                .child(div().flex().items_center().when_some(icon, |this, _icon| this))
                .child(div().flex_1().child(label))
                .cursor_pointer()
        });

        deferred(
            anchored()
                .snap_to_window_with_margin(px(8.))
                .anchor(Corner::TopLeft)
                .child(
                    v_flex()
                        .id("omnibox-dropdown")
                        .min_w(px(400.))
                        .max_h(px(320.))
                        .overflow_y_scroll()
                        .bg(cx.theme().colors().elevated_surface_background)
                        .border_1()
                        .border_color(cx.theme().colors().border)
                        .rounded_md()
                        .shadow_md()
                        .py_1()
                        .children(suggestions)
                )
        )
        .with_priority(1)
    }
}

fn looks_like_url(input: &str) -> bool {
    if input.starts_with("http://") || input.starts_with("https://") {
        return true;
    }
    // Contains :// scheme
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

fn text_to_url(text: &str) -> String {
    if text.starts_with("http://") || text.starts_with("https://") {
        return text.to_string();
    }

    if !looks_like_url(text) {
        let encoded: String = url::form_urlencoded::byte_serialize(text.as_bytes()).collect();
        return format!("https://www.google.com/search?q={encoded}");
    }

    if should_use_http_by_default(text) {
        format!("http://{text}")
    } else {
        format!("https://{text}")
    }
}

fn display_url(url: &str) -> String {
    if url == "glass://newtab" {
        return String::new();
    }

    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::{OmniboxSuggestion, looks_like_url, text_to_url};

    fn build_suggestions_for(query: &str) -> Vec<OmniboxSuggestion> {
        let mut suggestions = Vec::new();
        if looks_like_url(query) {
            suggestions.push(OmniboxSuggestion::RawUrl(query.to_string()));
            suggestions.push(OmniboxSuggestion::SearchQuery(query.to_string()));
        } else {
            suggestions.push(OmniboxSuggestion::SearchQuery(query.to_string()));
        }
        suggestions
    }

    #[test]
    fn url_like_input_is_first_suggestion() {
        let suggestions = build_suggestions_for("youtube.com/t3dotgg");
        assert!(
            matches!(&suggestions[0], OmniboxSuggestion::RawUrl(_)),
            "first suggestion should be RawUrl for URL-like input"
        );
        assert!(
            matches!(&suggestions[1], OmniboxSuggestion::SearchQuery(_)),
            "second suggestion should be SearchQuery as fallback"
        );
    }

    #[test]
    fn plain_query_puts_search_first() {
        let suggestions = build_suggestions_for("rust ownership");
        assert!(
            matches!(&suggestions[0], OmniboxSuggestion::SearchQuery(_)),
            "first suggestion should be SearchQuery for plain text"
        );
        assert_eq!(suggestions.len(), 1);
    }

    #[test]
    fn localhost_inputs_are_treated_as_urls() {
        assert!(looks_like_url("localhost"));
        assert!(looks_like_url("localhost:3000"));
        assert_eq!(text_to_url("localhost"), "http://localhost");
        assert_eq!(text_to_url("localhost:3000"), "http://localhost:3000");
    }

    #[test]
    fn regular_domains_default_to_https() {
        assert!(looks_like_url("example.com"));
        assert_eq!(text_to_url("example.com"), "https://example.com");
    }

    #[test]
    fn plain_queries_still_search() {
        assert!(!looks_like_url("rust ownership"));
        assert_eq!(
            text_to_url("rust ownership"),
            "https://www.google.com/search?q=rust+ownership"
        );
    }
}
