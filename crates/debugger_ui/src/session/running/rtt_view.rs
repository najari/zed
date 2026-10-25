//! RTT: the text of the target's RTT up channels, which probe-rs sends with its
//! `probe-rs-rtt-channel-config` and `probe-rs-rtt-data` events (not part of DAP). The session
//! keeps the lines; this view shows one channel and follows its end. In a session of several
//! cores each core has its own channels: a channel is a core and a channel number.

use gpui::{
    AnyElement, Empty, Entity, FocusHandle, Focusable, ListAlignment, ListState, Subscription, list,
};
use project::debugger::session::Session;
use ui::{Tooltip, prelude::*};

/// A channel: its core (in a session of several cores) and its number.
type ChannelKey = (Option<u64>, u32);

pub(crate) struct RttView {
    session: Entity<Session>,
    focus_handle: FocusHandle,
    /// The channel shown (the first one when none was picked).
    selected: Option<ChannelKey>,
    list: ListState,
    /// What the list shows: the channel, its dropped lines and its line count.
    shown: Option<(ChannelKey, usize, usize)>,
    _subscriptions: Vec<Subscription>,
}

impl RttView {
    pub(crate) fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            _subscriptions: vec![cx.observe(&session, |this, _, cx| {
                this.sync(cx);
                cx.notify();
            })],
            session,
            focus_handle: cx.focus_handle(),
            selected: None,
            list: ListState::new(0, ListAlignment::Bottom, px(1000.)),
            shown: None,
        };
        this.sync(cx);
        this
    }

    fn channel_key(&self, cx: &App) -> Option<ChannelKey> {
        let channels = self.session.read(cx).rtt_channels();
        self.selected
            .filter(|key| channels.iter().any(|c| (c.core, c.number) == *key))
            .or_else(|| channels.first().map(|c| (c.core, c.number)))
    }

    /// Brings the list up to the session's lines: new lines are added at the end (the list
    /// stays at the end when it was there), anything else starts it again.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.channel_key(cx) else {
            if self.shown.take().is_some() {
                self.list.reset(0);
            }
            return;
        };
        let session = self.session.read(cx);
        let Some(channel) = session
            .rtt_channels()
            .iter()
            .find(|c| (c.core, c.number) == key)
        else {
            return;
        };
        let now = (key, channel.dropped, channel.lines.len());
        match self.shown {
            Some((shown_key, dropped, len))
                if shown_key == key && dropped == now.1 && len <= now.2 =>
            {
                if now.2 > len {
                    self.list.splice(len..len, now.2 - len);
                }
            }
            _ => self.list.reset(now.2),
        }
        self.shown = Some(now);
    }

    fn select(&mut self, key: ChannelKey, cx: &mut Context<Self>) {
        self.selected = Some(key);
        self.sync(cx);
        cx.notify();
    }

    fn render_line(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(key) = self.channel_key(cx) else {
            return Empty.into_any();
        };
        let session = self.session.read(cx);
        let Some(line) = session
            .rtt_channels()
            .iter()
            .find(|c| (c.core, c.number) == key)
            .and_then(|c| c.lines.get(ix))
        else {
            return Empty.into_any();
        };
        div()
            .px_2()
            .child(
                Label::new(line.clone())
                    .buffer_font(cx)
                    .size(LabelSize::Small),
            )
            .into_any_element()
    }
}

impl Focusable for RttView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RttView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.channel_key(cx);
        let channels = self
            .session
            .read(cx)
            .rtt_channels()
            .iter()
            .map(|c| {
                (
                    (c.core, c.number),
                    c.name.clone(),
                    c.data_format.clone(),
                    c.lines.len(),
                )
            })
            .collect::<Vec<_>>();

        let toolbar = h_flex()
            .w_full()
            .gap_x_1()
            .px_1()
            .pb_1()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .children(channels.iter().map(|(key, name, format, lines)| {
                let key = *key;
                let (core, number) = key;
                let channel = if name.is_empty() {
                    format!("{number}")
                } else {
                    format!("{number}: {name}")
                };
                let label = match core {
                    Some(core) => format!("core {core} · {channel}"),
                    None => channel,
                };
                let id = format!("rtt-channel-{}-{number}", core.map_or(-1, |c| c as i64));
                Button::new(SharedString::from(id), label)
                    .label_size(LabelSize::Small)
                    .toggle_state(shown == Some(key))
                    .tooltip(Tooltip::text(format!("{format}, {lines} lines")))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(key, cx)))
            }))
            .child(div().flex_1())
            .when_some(shown, |this, (core, number)| {
                this.child(
                    IconButton::new("rtt-clear", IconName::Trash)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("Clear"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.session.update(cx, |session, cx| {
                                session.clear_rtt_channel(core, number, cx)
                            });
                        })),
                )
            });

        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .p_1()
            .child(toolbar)
            .map(|this| {
                if channels.is_empty() {
                    this.child(
                        div().p_1().child(
                            Label::new(
                                "No RTT channel yet. RTT needs rttEnabled in coreConfigs \
                                 and an RTT control block in the program.",
                            )
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        ),
                    )
                } else {
                    this.child(
                        list(
                            self.list.clone(),
                            cx.processor(|this, ix, _window, cx| this.render_line(ix, cx)),
                        )
                        .flex_1()
                        .size_full(),
                    )
                }
            })
    }
}
