//! Live Watch: global variables read while the target runs, with the probe-rs `liveWatch`
//! request (not part of DAP). The expressions are read every [`INTERVAL`]; in a session of
//! several cores (one thread per core) they are read on every core and grouped by core. A value
//! that changed since the last read is marked.

use std::time::Duration;

use collections::{HashMap, HashSet};
use db::kvp::KeyValueStore;
use editor::Editor;
use gpui::{Entity, FocusHandle, Focusable, Subscription, Task};
use menu::Confirm;
use project::debugger::{dap_command::LiveWatchValue, session::Session};
use ui::{Tooltip, prelude::*};

/// How often the expressions are read.
const INTERVAL: Duration = Duration::from_millis(500);
/// Where the expressions are kept between sessions.
const EXPRESSIONS_KEY: &str = "debugger_live_watch_expressions";

/// What the view reads on: the session, or one core of it.
struct Target {
    /// The core (thread id) in a session of several cores.
    core: Option<u64>,
    label: SharedString,
    values: HashMap<String, LiveWatchValue>,
    /// The expressions whose value changed at the last read.
    changed: HashSet<String>,
    error: Option<SharedString>,
    /// A read is under way (a slow probe must not pile up requests).
    busy: bool,
}

impl Target {
    fn new(core: Option<u64>, label: SharedString) -> Self {
        Self {
            core,
            label,
            values: HashMap::default(),
            changed: HashSet::default(),
            error: None,
            busy: false,
        }
    }
}

pub(crate) struct LiveWatchView {
    session: Entity<Session>,
    focus_handle: FocusHandle,
    input: Entity<Editor>,
    expressions: Vec<String>,
    targets: Vec<Target>,
    paused: bool,
    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl LiveWatchView {
    pub(crate) fn new(
        session: Entity<Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Add a global variable to watch", window, cx);
            editor
        });
        let expressions = KeyValueStore::global(cx)
            .read_kvp(EXPRESSIONS_KEY)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let _poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(INTERVAL).await;
                let Ok(running) = this.update(cx, |this, cx| this.refresh(cx)) else {
                    break;
                };
                if !running {
                    break;
                }
            }
        });
        Self {
            _subscriptions: vec![cx.observe(&session, |_, _, cx| cx.notify())],
            session,
            focus_handle: cx.focus_handle(),
            input,
            expressions,
            targets: Vec::new(),
            paused: false,
            _poll,
        }
    }

    /// The targets to read on: one per thread when the session has several (each thread is a
    /// core), else the session.
    fn sync_targets(&mut self, cx: &mut Context<Self>) {
        let threads = self.session.update(cx, |session, cx| session.threads(cx));
        let wanted: Vec<(Option<u64>, SharedString)> = if threads.len() > 1 {
            threads
                .iter()
                .filter_map(|(thread, _)| {
                    Some((
                        Some(u64::try_from(thread.id).ok()?),
                        thread.name.clone().into(),
                    ))
                })
                .collect()
        } else {
            vec![(None, SharedString::default())]
        };
        if wanted.len() == self.targets.len()
            && wanted
                .iter()
                .zip(&self.targets)
                .all(|((core, _), target)| *core == target.core)
        {
            return;
        }
        let mut old = std::mem::take(&mut self.targets);
        self.targets = wanted
            .into_iter()
            .map(
                |(core, label)| match old.iter().position(|target| target.core == core) {
                    Some(ix) => old.swap_remove(ix),
                    None => Target::new(core, label),
                },
            )
            .collect();
    }

    /// Reads the expressions on every target that is not reading already. Returns whether the
    /// session still runs (the view stops reading when it ended).
    fn refresh(&mut self, cx: &mut Context<Self>) -> bool {
        if self.session.read(cx).is_terminated() {
            return false;
        }
        if self.paused || self.expressions.is_empty() || !self.session.read(cx).is_started() {
            return true;
        }
        self.sync_targets(cx);
        for ix in 0..self.targets.len() {
            let target = &mut self.targets[ix];
            if target.busy {
                continue;
            }
            target.busy = true;
            let core = target.core;
            let request = self
                .session
                .read(cx)
                .live_watch(self.expressions.clone(), core);
            cx.spawn(async move |this, cx| {
                let result = request.await;
                this.update(cx, |this, cx| {
                    let Some(target) = this.targets.iter_mut().find(|t| t.core == core) else {
                        return;
                    };
                    target.busy = false;
                    match result {
                        Ok(values) => {
                            let values: HashMap<String, LiveWatchValue> = values
                                .into_iter()
                                .map(|value| (value.expression.clone(), value))
                                .collect();
                            target.changed = values
                                .iter()
                                .filter(|(expression, value)| {
                                    target
                                        .values
                                        .get(*expression)
                                        .is_some_and(|old| old.value != value.value)
                                })
                                .map(|(expression, _)| expression.clone())
                                .collect();
                            target.values = values;
                            target.error = None;
                        }
                        Err(error) => target.error = Some(format!("{error:#}").into()),
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        true
    }

    fn save(&self, cx: &mut Context<Self>) {
        let Ok(text) = serde_json::to_string(&self.expressions) else {
            return;
        };
        let kvp = KeyValueStore::global(cx);
        cx.background_spawn(async move { kvp.write_kvp(EXPRESSIONS_KEY.to_string(), text).await })
            .detach_and_log_err(cx);
    }

    fn add(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let expression = self.input.update(cx, |editor, cx| {
            let text = editor.text(cx);
            editor.clear(window, cx);
            text
        });
        let expression = expression.trim().to_string();
        if expression.is_empty() || self.expressions.contains(&expression) {
            return;
        }
        self.expressions.push(expression);
        self.save(cx);
        self.refresh(cx);
        cx.notify();
    }

    fn remove(&mut self, expression: &str, cx: &mut Context<Self>) {
        self.expressions.retain(|e| e != expression);
        self.save(cx);
        cx.notify();
    }

    fn render_value(&self, target: &Target, expression: &str, cx: &App) -> AnyElement {
        if let Some(error) = &target.error {
            return Label::new(format!("error: {error}"))
                .size(LabelSize::Small)
                .color(Color::Error)
                .into_any_element();
        }
        match target.values.get(expression) {
            None => Label::new(if self.paused { "paused" } else { "…" })
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element(),
            Some(LiveWatchValue {
                value: Some(value),
                type_,
                ..
            }) => h_flex()
                .gap_x_2()
                .child(
                    Label::new(value.clone())
                        .size(LabelSize::Small)
                        .buffer_font(cx)
                        .color(if target.changed.contains(expression) {
                            Color::Warning
                        } else {
                            Color::Default
                        }),
                )
                .when_some(type_.clone(), |this, type_| {
                    this.child(
                        Label::new(type_)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                })
                .into_any_element(),
            Some(LiveWatchValue { error, .. }) => {
                Label::new(format!("<{}>", error.clone().unwrap_or_default()))
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .into_any_element()
            }
        }
    }

    fn render_rows(&self, target: Option<&Target>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.expressions
            .iter()
            .enumerate()
            .map(|(ix, expression)| {
                let key = (target.and_then(|t| t.core).unwrap_or(u64::MAX), ix);
                h_flex()
                    .id(SharedString::from(format!(
                        "live-watch-{}-{}",
                        key.0, key.1
                    )))
                    .w_full()
                    .gap_x_2()
                    .px_2()
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .child(
                        div().min_w(px(120.)).child(
                            Label::new(expression.clone())
                                .size(LabelSize::Small)
                                .buffer_font(cx)
                                .color(Color::Accent),
                        ),
                    )
                    .child(
                        div().flex_1().child(match target {
                            Some(target) => self.render_value(target, expression, cx),
                            None => Label::new("no session")
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .into_any_element(),
                        }),
                    )
                    .child({
                        let expression = expression.clone();
                        IconButton::new(
                            SharedString::from(format!("live-watch-remove-{}-{}", key.0, key.1)),
                            IconName::Close,
                        )
                        .icon_size(IconSize::XSmall)
                        .tooltip(Tooltip::text("Remove"))
                        .on_click(cx.listener(move |this, _, _, cx| this.remove(&expression, cx)))
                    })
                    .into_any_element()
            })
            .collect()
    }
}

impl Focusable for LiveWatchView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for LiveWatchView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let paused = self.paused;
        let toolbar = h_flex()
            .w_full()
            .gap_x_1()
            .px_1()
            .pb_1()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                div()
                    .flex_1()
                    .key_context("LiveWatch")
                    .on_action(cx.listener(Self::add))
                    .child(self.input.clone()),
            )
            .child(
                IconButton::new(
                    "live-watch-pause",
                    if paused {
                        IconName::PlayFilled
                    } else {
                        IconName::DebugPause
                    },
                )
                .icon_size(IconSize::Small)
                .tooltip(Tooltip::text(if paused {
                    "Resume Reading"
                } else {
                    "Pause Reading"
                }))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.paused = !this.paused;
                    cx.notify();
                })),
            );

        let mut body = v_flex().w_full().gap_y_0p5();
        if self.expressions.is_empty() {
            body = body.child(
                Label::new("Type a global variable above and press Enter")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            );
        } else if self.targets.len() > 1 {
            for ix in 0..self.targets.len() {
                let target = &self.targets[ix];
                let header = Label::new(target.label.clone())
                    .size(LabelSize::Small)
                    .color(Color::Muted);
                let rows = self.render_rows(Some(&self.targets[ix]), cx);
                body = body.child(div().pt_1().px_1().child(header)).children(rows);
            }
        } else {
            let rows = self.render_rows(self.targets.first(), cx);
            body = body.children(rows);
        }

        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .p_1()
            .child(toolbar)
            .child(
                div()
                    .id("live-watch-body")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(body),
            )
    }
}
