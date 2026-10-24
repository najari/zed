//! Disassembly around the program counter of the selected stack frame, from the adapter's
//! `disassemble` request. The row of the program counter is marked and kept in view. A click
//! in the left column of a row sets or removes a breakpoint on that instruction, and the
//! toolbar steps one instruction and reads more instructions before or after the shown ones.

use dap::{DisassembledInstruction, SteppingGranularity};
use gpui::{
    AnyElement, Empty, Entity, FocusHandle, Focusable, ListOffset, ListState, Subscription, Task,
    WeakEntity, list,
};
use project::debugger::session::{Session, ThreadStatus};
use ui::{Tooltip, prelude::*};

use super::RunningState;
use super::stack_frame_list::{StackFrameList, StackFrameListEvent};

/// Instructions shown before the program counter.
const BEFORE: i64 = 24;
/// Instructions asked for in all (before, at and after the program counter).
const COUNT: u64 = 80;
/// Instructions added by one "earlier" or "later".
const MORE: u64 = 40;

pub(crate) struct DisassemblyView {
    session: Entity<Session>,
    running_state: WeakEntity<RunningState>,
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
    /// Reading more instructions before or after the shown ones.
    _fetch_more: Task<()>,
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

#[derive(Clone, Copy)]
enum Side {
    Before,
    After,
}

impl DisassemblyView {
    pub(crate) fn new(
        session: Entity<Session>,
        running_state: WeakEntity<RunningState>,
        stack_frame_list: Entity<StackFrameList>,
        cx: &mut Context<Self>,
    ) -> Self {
        let _subscriptions = vec![
            cx.subscribe(&stack_frame_list, |this, _, event, cx| match event {
                StackFrameListEvent::SelectedStackFrameChanged(_)
                | StackFrameListEvent::BuiltEntries => this.refresh(cx),
            }),
            // Breakpoint states and thread states.
            cx.observe(&session, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            session,
            running_state,
            stack_frame_list,
            focus_handle: cx.focus_handle(),
            list: ListState::new(0, gpui::ListAlignment::Top, px(1000.)),
            frame_id: None,
            pc: None,
            instructions: Vec::new(),
            message: Some("Not stopped".into()),
            scroll_to_pc: false,
            _fetch: Task::ready(()),
            _fetch_more: Task::ready(()),
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
        self._fetch_more = Task::ready(());
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

    /// Reads `MORE` instructions before the first shown one or after the last, and adds the ones
    /// not shown yet. The view stays where it is.
    fn fetch_more(&mut self, side: Side, cx: &mut Context<Self>) {
        let edge = match side {
            Side::Before => self.instructions.first(),
            Side::After => self.instructions.last(),
        };
        let Some(edge) = edge else {
            return;
        };
        let Some(edge_address) = parse_address(&edge.address) else {
            return;
        };
        let reference = edge.address.clone();
        let offset = match side {
            Side::Before => -(MORE as i64),
            Side::After => 1,
        };
        let request = self.session.update(cx, |session, cx| {
            session.disassemble(reference, offset, MORE, cx)
        });
        self._fetch_more = cx.spawn(async move |this, cx| {
            let Some(instructions) = request.await else {
                return;
            };
            this.update(cx, |this, cx| {
                // An adapter may answer with instructions that overlap the shown ones (finding the
                // instruction starts before an address is a guess on a variable length
                // instruction set): keep only the ones beyond the edge.
                let beyond = |instruction: &DisassembledInstruction| {
                    parse_address(&instruction.address).is_some_and(|address| match side {
                        Side::Before => address < edge_address,
                        Side::After => address > edge_address,
                    })
                };
                let new = instructions.into_iter().filter(beyond).collect::<Vec<_>>();
                let count = new.len();
                match side {
                    Side::Before => {
                        let shown = std::mem::replace(&mut this.instructions, new);
                        this.instructions.extend(shown);
                        this.list.splice(0..0, count);
                    }
                    Side::After => {
                        let end = this.instructions.len();
                        this.instructions.extend(new);
                        this.list.splice(end..end, count);
                    }
                }
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

    fn supports_instruction_breakpoints(&self, cx: &App) -> bool {
        self.session
            .read(cx)
            .capabilities()
            .supports_instruction_breakpoints
            .unwrap_or_default()
    }

    fn toggle_breakpoint(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(instruction) = self.instructions.get(ix) else {
            return;
        };
        let reference = instruction.address.clone();
        self.session.update(cx, |session, cx| {
            session.toggle_instruction_breakpoint(reference, cx)
        });
    }

    fn thread_stopped(&self, cx: &App) -> bool {
        self.running_state
            .upgrade()
            .and_then(|state| state.read(cx).thread_status(cx))
            == Some(ThreadStatus::Stopped)
    }

    /// Steps the selected thread by one instruction: over calls (`step_in` false) or into them.
    fn step_instruction(&mut self, step_in: bool, cx: &mut Context<Self>) {
        let Some(state) = self.running_state.upgrade() else {
            return;
        };
        let Some(thread_id) = state.read(cx).thread_id else {
            return;
        };
        self.session.update(cx, |session, cx| {
            if step_in {
                session.step_in(thread_id, SteppingGranularity::Instruction, cx);
            } else {
                session.step_over(thread_id, SteppingGranularity::Instruction, cx);
            }
        });
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let stopped = self.thread_stopped(cx);
        let shown = !self.instructions.is_empty();
        h_flex()
            .w_full()
            .gap_x_1()
            .px_1()
            .pb_1()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                IconButton::new("disassembly-step-over", IconName::DebugStepOver)
                    .icon_size(IconSize::Small)
                    .disabled(!stopped)
                    .tooltip(Tooltip::text("Step Over Instruction"))
                    .on_click(cx.listener(|this, _, _, cx| this.step_instruction(false, cx))),
            )
            .child(
                IconButton::new("disassembly-step-into", IconName::DebugStepInto)
                    .icon_size(IconSize::Small)
                    .disabled(!stopped)
                    .tooltip(Tooltip::text("Step Into Instruction"))
                    .on_click(cx.listener(|this, _, _, cx| this.step_instruction(true, cx))),
            )
            .child(div().w(px(8.)))
            .child(
                IconButton::new("disassembly-earlier", IconName::ArrowUp)
                    .icon_size(IconSize::Small)
                    .disabled(!shown)
                    .tooltip(Tooltip::text("Read Earlier Instructions"))
                    .on_click(cx.listener(|this, _, _, cx| this.fetch_more(Side::Before, cx))),
            )
            .child(
                IconButton::new("disassembly-later", IconName::ArrowDown)
                    .icon_size(IconSize::Small)
                    .disabled(!shown)
                    .tooltip(Tooltip::text("Read Later Instructions"))
                    .on_click(cx.listener(|this, _, _, cx| this.fetch_more(Side::After, cx))),
            )
            .child(
                IconButton::new("disassembly-go-to-pc", IconName::Crosshair)
                    .icon_size(IconSize::Small)
                    .disabled(self.pc_index().is_none())
                    .tooltip(Tooltip::text("Show Program Counter"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.scroll_to_pc = true;
                        cx.notify();
                    })),
            )
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
        let breakpoint = self
            .session
            .read(cx)
            .instruction_breakpoint(&instruction.address)
            .cloned();
        let breakpoints = self.supports_instruction_breakpoints(cx) && !invalid;
        // The source line, where it starts (the adapter gives it on the first instruction of
        // a line).
        let line = instruction.line.filter(|line| {
            ix == 0 || self.instructions.get(ix - 1).and_then(|prev| prev.line) != Some(*line)
        });
        let file = instruction
            .location
            .as_ref()
            .and_then(|source| source.name.clone());

        let gutter = div()
            .id(("disassembly-breakpoint", ix))
            .w(px(14.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .when_some(breakpoint, |this, breakpoint| {
                let (icon, color) = match breakpoint.verified {
                    Some(true) => (IconName::DebugBreakpoint, Color::Error),
                    _ => (IconName::DebugDisabledBreakpoint, Color::Muted),
                };
                let tooltip = match (breakpoint.verified, breakpoint.message) {
                    (_, Some(message)) => message,
                    (None, None) => "Setting the breakpoint".to_string(),
                    (Some(true), None) => "Instruction breakpoint".to_string(),
                    (Some(false), None) => "The adapter did not set the breakpoint".to_string(),
                };
                this.child(Icon::new(icon).size(IconSize::XSmall).color(color))
                    .tooltip(Tooltip::text(tooltip))
            })
            .when(breakpoints, |this| {
                this.cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_breakpoint(ix, cx)))
            });

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
                    .child(gutter)
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
        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .p_1()
            .child(self.render_toolbar(cx))
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
                    .flex_1()
                    .size_full(),
                ),
            })
    }
}
