//! A gpui-kit to-do app that stock Playwright drives through gpui-playwright.
//!
//! ```sh
//! GPUI_PLAYWRIGHT=9333 cargo run -p todo
//! ```
//!
//! Then connect any Playwright client to the printed `ws://` endpoint, or run
//! the suite in `e2e/`, which starts the app itself.

use gpui_kit::{
    component::{
        ActiveTheme, IconName, Sizable, Theme, ThemeMode,
        avatar::Avatar,
        button::{Button, ButtonVariants},
        checkbox::Checkbox,
        input::{Input, InputEvent, InputState},
        label::Label,
    },
    *,
};

#[derive(Clone)]
struct Todo {
    id: usize,
    title: String,
    done: bool,
}

#[derive(Default)]
struct TodoStore {
    todos: Vec<Todo>,
    next_id: usize,
}

impl TodoStore {
    /// The state the app launches with, and every Playwright context starts from.
    fn seeded() -> Self {
        let mut store = Self::default();
        store.add("Read the docs");
        store.add("Write a Playwright test");
        store.todos[0].done = true;
        store
    }

    fn add(&mut self, title: &str) -> bool {
        let title = title.trim();
        if title.is_empty() {
            return false;
        }
        self.next_id += 1;
        self.todos.push(Todo {
            id: self.next_id,
            title: title.to_owned(),
            done: false,
        });
        true
    }

    fn remaining(&self) -> usize {
        self.todos.iter().filter(|todo| !todo.done).count()
    }
}

struct Store(Entity<TodoStore>);
impl Global for Store {}

fn store(cx: &App) -> Entity<TodoStore> {
    cx.global::<Store>().0.clone()
}

struct TodoView {
    input: Entity<InputState>,
}

impl TodoView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // GPUI's headless platform ignores `TitlebarOptions::title`.
        window.set_window_title("Todos");
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("What needs to be done?"));
        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.add(window, cx);
            }
        })
        .detach();
        cx.observe(&store(cx), |_, _, cx| cx.notify()).detach();
        Self { input }
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.input.read(cx).value().to_string();
        let added = store(cx).update(cx, |store, cx| {
            let added = store.add(&title);
            cx.notify();
            added
        });
        if added {
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }
}

impl Render for TodoView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = store(cx).read(cx);
        let todos = store.todos.clone();
        let left = format!("{} of {} left", store.remaining(), store.todos.len());
        let theme = cx.theme();
        let dark = theme.mode.is_dark();

        div()
            .id("todo-root")
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div()
                    .h_10()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_3()
                            .child(Label::new("Todos").text_2xl().font_weight(FontWeight::BOLD))
                            .child(
                                div()
                                    .id("remaining")
                                    .test_support()
                                    .aria_label(left.clone())
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(left),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Avatar::new().name("Ada Lovelace").small())
                            .child(
                                Button::new("theme-toggle")
                                    .ghost()
                                    .tooltip("Switch theme")
                                    .label(if dark { "Light" } else { "Dark" })
                                    .icon(if dark { IconName::Sun } else { IconName::Moon })
                                    .on_click(move |_, window, cx| {
                                        let mode = if dark {
                                            ThemeMode::Light
                                        } else {
                                            ThemeMode::Dark
                                        };
                                        Theme::change(mode, Some(window), cx);
                                        cx.refresh_windows();
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.input).id("new-todo")))
                    .child(
                        Button::new("add")
                            .primary()
                            .icon(IconName::Plus)
                            .label("Add")
                            .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                    ),
            )
            .child(
                div()
                    .id("todo-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(todos.into_iter().map(|todo| {
                        let id = todo.id;
                        div()
                            .id(("row", id))
                            .h_11()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_2()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                Checkbox::new(("done", id))
                                    .checked(todo.done)
                                    .label(todo.title.clone())
                                    .on_click(move |checked, _, cx| {
                                        let checked = *checked;
                                        self::store(cx).update(cx, |store, cx| {
                                            if let Some(todo) =
                                                store.todos.iter_mut().find(|t| t.id == id)
                                            {
                                                todo.done = checked;
                                            }
                                            cx.notify();
                                        });
                                    }),
                            )
                            .child(
                                Button::new(("delete", id))
                                    .ghost()
                                    .small()
                                    .label("Delete")
                                    .on_click(move |_, _, cx| {
                                        self::store(cx).update(cx, |store, cx| {
                                            store.todos.retain(|t| t.id != id);
                                            cx.notify();
                                        });
                                    }),
                            )
                    })),
            )
    }
}

fn open_main_window(cx: &mut App) -> Option<AnyWindowHandle> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(560.), px(480.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Todos".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    match gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| TodoView::new(window, cx))
    }) {
        Ok((window, _)) => Some(window),
        Err(err) => {
            eprintln!("failed to open the window: {err}");
            None
        }
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            let todos = cx.new(|_| TodoStore::seeded());
            cx.set_global(Store(todos));
            open_main_window(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            #[cfg(feature = "playwright")]
            if let Some(server) = gpui_playwright::Server::from_env() {
                let server = server
                    // Each test gets a fresh list.
                    .on_new_context(|cx| {
                        self::store(cx).update(cx, |store, cx| {
                            *store = TodoStore::seeded();
                            cx.notify();
                        });
                    })
                    // A deep link, so tests can `page.goto('todo://clear-completed')`.
                    .on_navigate(|url, _, cx| match url {
                        "todo://clear-completed" => {
                            self::store(cx).update(cx, |store, cx| {
                                store.todos.retain(|todo| !todo.done);
                                cx.notify();
                            });
                            Ok(())
                        }
                        _ => Err(format!("no route for {url}")),
                    })
                    .on_color_scheme(|scheme, cx| {
                        let mode = match scheme {
                            gpui_playwright::ColorScheme::Dark => ThemeMode::Dark,
                            _ => ThemeMode::Light,
                        };
                        Theme::change(mode, None, cx);
                        cx.refresh_windows();
                    });
                if let Err(err) = server.start(cx) {
                    eprintln!("gpui-playwright failed to start: {err}");
                    cx.quit();
                }
            }
        });
}
