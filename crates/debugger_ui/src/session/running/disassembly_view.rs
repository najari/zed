//! Disassembly around the program counter of the selected stack frame, from the adapter's
//! `disassemble` request. The row of the program counter is marked and kept in view.

use dap::DisassembledInstruction;
use gpui::{
    AnyElement, Empty, Entity, FocusHandle, Focusable, ListOffset, ListState, Subscription, Task,
    list,
};
use project::debugger::session::Session;
use ui::prelude::*;

use super::stack_frame_list::{StackFrameList, StackFrameListEvent};

/// Instructions shown before the program counter.
const BEFORE: i64 = 24;
/// Instructions asked for in all (before, at and after the program counter).
const COUNT: u64 = 80;

pub(crate) struct DisassemblyView {
    session: Entity<Session>,
    stack_frame_list: Entity<StackFrameList>,
    focus_handle: FocusHandle,
    list: ListState,
    /// The stack frame the instructions were asked for. The adapter hands out new frame ids at
    /// every stop, so an old frame (still listed right after a stop) is not asked for again.
    frame_id: Option<dap::StackFrameId>,
    pc: Option<u64>,
    instructions: Vec<DisassembledInstruction>,
    message: Option<SharedString>,
    /// Bring the program counter into view at the next render (the list has no layout before
    /// the panel is shown).
    scroll_to_pc: bool,
    _fetch: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// An address as DAP writes it: hex with `0x`, else decimal.
fn parse_address(text: &str) -> Option<u64> {
    let text = text.trim();
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

impl DisassemblyView {
    pub(crate) fn new(
        session: Entity<Session>,
        stack_frame_list: Entity<StackFrameList>,
        cx: &mut Context<Self>,
    ) -> Self {
        let _subscriptions =
            vec![
                cx.subscribe(&stack_frame_list, |this, _, event, cx| match event {
                    StackFrameListEvent::SelectedStackFrameChanged(_)
                    | StackFrameListEvent::BuiltEntries => this.refresh(cx),
                }),
            ];
        let mut this = Self {
            session,
            stack_frame_list,
            focus_handle: cx.focus_handle(),
            list: ListState::new(0, gpui::ListAlignment::Top, px(1000.)),
            frame_id: None,
            pc: None,
            instructions: Vec::new(),
            message: Some("Not stopped".into()),
            scroll_to_pc: false,
            _fetch: Task::ready(()),
            _subscriptions,
        };
        this.refresh(cx);
        this
    }

    /// Asks for the instructions around the program counter of the selected stack frame (the
    /// first frame when none is selected), unless they are already shown.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let frame = self.stack_frame_list.read(cx).current_stack_frame();
        let Some(frame) = frame else {
            return;
        };
        if self.frame_id == Some(frame.id) {
            return;
        }
        self.frame_id = Some(frame.id);
        let Some(reference) = frame.instruction_pointer_reference.clone() else {
            self.message = Some("The stack frame has no instruction pointer".into());
            self.instructions.clear();
            self.list.reset(0);
            cx.notify();
            return;
        };
        self.pc = parse_address(&reference);
        let request = self.session.update(cx, |session, cx| {
            session.disassemble(reference, -BEFORE, COUNT, cx)
        });
        self._fetch = cx.spawn(async move |this, cx| {
            let instructions = request.await;
            this.update(cx, |this, cx| {
                match instructions {
                    Some(instructions) => {
                        this.message = None;
                        this.instructions = instructions;
                    }
                    None => {
                        this.message = Some("The adapter could not disassemble here".into());
                        this.instructions.clear();
                    }
                }
                this.list.reset(this.instructions.len());
                this.scroll_to_pc = true;
                cx.notify();
            })
            .ok();
        });
    }

    fn pc_index(&self) -> Option<usize> {
        let pc = self.pc?;
        self.instructions
            .iter()
            .position(|instruction| parse_address(&instruction.address) == Some(pc))
    }

    fn render_entry(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(instruction) = self.instructions.get(ix) else {
            return Empty.into_any();
        };
        let address = parse_address(&instruction.address);
        let at_pc = address.is_some() && address == self.pc;
        let invalid = matches!(
            instruction.presentation_hint,
            Some(dap::DisassembledInstructionPresentationHint::Invalid)
        );
        // The source line, where it starts (the adapter gives it on the first instruction of
        // a line).
        let line = instruction.line.filter(|line| {
            ix == 0 || self.instructions.get(ix - 1).and_then(|prev| prev.line) != Some(*line)
        });
        let file = instruction
            .location
            .as_ref()
            .and_then(|source| source.name.clone());

        v_flex()
            .w_full()
            .when_some(line, |this, line| {
                this.child(
                    h_flex().pt_1().px_1().child(
                        Label::new(match &file {
                            Some(file) => format!("{file}:{line}"),
                            None => format!("line {line}"),
                        })
                        .size(LabelSize::XSmall)
                        .color(Color::Accent),
                    ),
                )
            })
            .child(
                h_flex()
                    .id(("disassembly-row", ix))
                    .w_full()
                    .gap_x_2()
                    .px_1()
                    .when(at_pc, |this| {
                        this.bg(cx.theme().colors().editor_debugger_active_line_background)
                    })
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .child(
                        div().w(px(12.)).child(
                            Label::new(if at_pc { "\u{25B6}" } else { "" })
                                .size(LabelSize::XSmall)
                                .color(Color::Warning),
                        ),
                    )
                    .child(
                        Label::new(instruction.address.clone())
                            .buffer_font(cx)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        div().w(px(96.)).child(
                            Label::new(instruction.instruction_bytes.clone().unwrap_or_default())
                                .buffer_font(cx)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                    )
                    .child(
                        Label::new(instruction.instruction.clone())
                            .buffer_font(cx)
                            .size(LabelSize::Small)
                            .when(invalid, |this| this.color(Color::Disabled)),
                    )
                    .when_some(instruction.symbol.clone(), |this, symbol| {
                        this.child(
                            Label::new(format!("<{symbol}>"))
                                .buffer_font(cx)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                    }),
            )
            .into_any()
    }
}

impl Focusable for DisassemblyView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DisassemblyView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.scroll_to_pc {
            self.scroll_to_pc = false;
            if let Some(ix) = self.pc_index() {
                // A few instructions of context above the program counter.
                self.list.scroll_to(ListOffset {
                    item_ix: ix.saturating_sub(6),
                    offset_in_item: px(0.),
                });
            }
        }
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .p_1()
            .map(|this| match self.message.clone() {
                Some(message) if self.instructions.is_empty() => this.child(
                    Label::new(message)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
                _ => this.child(
                    list(
                        self.list.clone(),
                        cx.processor(|this, ix, _window, cx| this.render_entry(ix, cx)),
                    )
                    .size_full(),
                ),
            })
    }
}
