//! Validation-only input driver for the existing real product Window.
//!
//! This observes existing service/feedback values. It cannot construct a GUI
//! claim, answer a Question, publish Presented, or interpret working pixels.

use super::host::AppUiHost;
use super::rendering::AppUiFrameResult;
use crate::app::preview_native_gui::NativeVisualQuestionPresentation;
use crate::app::AppState;
use mondrian_core::{ClipId, EffectId, SequenceId};
use mondrian_effects::{EffectProcessorQuestionReply, EffectProcessorQuestionState};
use mondrian_ui_core::{types::*, Widget};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// One separate process run exercises one actual activation path.
#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeQuestionWindowInteraction {
    /// Traverse modal focus both ways, then activate Yes with Enter down/up.
    KeyboardYes,
    /// Traverse modal focus both ways, then click the actual No control.
    PointerNo,
    /// Traverse modal focus both ways, then dismiss with routed Escape.
    EscapeDismiss,
}
impl NativeQuestionWindowInteraction {
    fn expected(self) -> EffectProcessorQuestionReply {
        match self {
            Self::KeyboardYes => EffectProcessorQuestionReply::Yes,
            Self::PointerNo => EffectProcessorQuestionReply::No,
            Self::EscapeDismiss => EffectProcessorQuestionReply::Dismissed,
        }
    }
}

/// Explicit authored target and real project/bundle inputs; none grants GUI authority.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeQuestionWindowRequest {
    /// Existing canonical project containing this exact authored effect.
    pub project: PathBuf,
    /// Actual owned native bundle registered through the normal product action.
    pub bundle: PathBuf,
    /// Exact active Sequence expected in the opened project.
    pub sequence: SequenceId,
    /// Exact selected Clip, visible at the current project playhead.
    pub clip: ClipId,
    /// Exact existing authored native effect occurrence.
    pub effect: EffectId,
    /// Real router activation used by this run.
    pub interaction: NativeQuestionWindowInteraction,
    /// Absolute validation timeout, including native completion, in milliseconds.
    pub timeout_ms: u64,
}
impl NativeQuestionWindowRequest {
    pub(super) fn deadline(&self, now: Instant) -> Result<Instant, String> {
        if self.timeout_ms == 0 {
            return Err("Question validation timeout must be nonzero".to_owned());
        }
        now.checked_add(Duration::from_millis(self.timeout_ms))
            .ok_or_else(|| "Question validation deadline overflow".to_owned())
    }
}

/// Read-only input from one actual Window frame, sampled before its surface gate.
pub(crate) struct NativeQuestionSurfaceFrame {
    pub(crate) state_before: Option<EffectProcessorQuestionState>,
    pub(crate) result: AppUiFrameResult,
}

/// Read-only facts from one real paint, sampled around the product surface gate.
/// These diagnostics grant no input, GUI or Presented authority.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct NativeQuestionSurfaceObservation {
    /// Real Window identity for this paint, even without a Question.
    pub window: String,
    /// Real surface generation for this paint.
    pub surface_generation: u64,
    /// Actual adapter used for this paint.
    pub adapter: String,
    /// Whether the product tree painted an actual opaque Question delivery.
    pub painted_question: bool,
    /// State sampled before the existing product presentation gate.
    pub state_before: Option<String>,
    /// State sampled after that gate; Pending/dismissal is not presentation.
    pub state_after: Option<String>,
    /// Exact original owner/service coordinates, excluding plugin text.
    pub receipt: Option<String>,
    /// Actual Window renderer result, including upload and resource diagnostics.
    pub frame_result: String,
    /// Actual Preview/section/demand facts; this read does not admit or retry work.
    pub preview_observation: String,
}

/// Bounded observations from real input and the original native completion.
/// No field asserts float pixel acceptance or physical OS process termination.
#[derive(Debug, Default, serde::Serialize)]
pub struct NativeQuestionWindowReport {
    /// All operation and consuming Rust-owner shutdown checks passed.
    pub passed: bool,
    /// Exact typed input mode requested by the caller.
    pub interaction: Option<NativeQuestionWindowInteraction>,
    /// Real native Window identity, not a simulated surface.
    pub window_id: Option<String>,
    /// Actual surface generation that published the admitted modal frame.
    pub surface_generation: Option<u64>,
    /// Actual graphics adapter selected by the product Window.
    pub adapter: Option<String>,
    /// Actual modal scope/control IDs from its current layout.
    pub modal_controls: Option<[String; 4]>,
    /// Exact original supervised child PID (in-process output cannot qualify).
    pub native_pid: Option<u32>,
    /// Actual parent processor owner and native service coordinates.
    pub parent_owner: Option<String>,
    /// Child-local actual processor owner.
    pub native_owner: Option<u64>,
    /// Original Render invocation.
    pub invocation: Option<u64>,
    /// Original Question service serial.
    pub service: Option<u64>,
    /// Original exact current work scope and bank preparation, diagnostics only.
    pub work: Option<String>,
    /// Real surface result admitted by the existing Host presentation gate.
    pub presented: Option<String>,
    /// At most sixteen distinct read-only paint observations; the last is retained.
    pub surface_observations: Vec<NativeQuestionSurfaceObservation>,
    /// Distinct middle observations replaced after the fixed sixteen-record bound.
    pub surface_observations_replaced: u64,
    /// One record after each separately routed/down-drained/up-drained event.
    pub events: Vec<String>,
    /// Exact reply sampled immediately after the actual routed activation.
    pub reply: Option<String>,
    /// Existing current feedback receipt matching the original native invocation.
    pub completed: Option<String>,
    /// Canonical author generation at admission.
    pub author_generation: u64,
    /// Whether all actual Window Rust-owned authorities were consumed.
    pub window_owners_released: bool,
    /// Whether all actual App workers/resources were consumed.
    pub app_owners_released: bool,
    /// Whether the process-local EventLoop Rust owner was consumed.
    pub event_loop_owner_released: bool,
    /// Actual consuming Window evidence (does not claim OS termination).
    pub window_shutdown: Option<String>,
    /// Actual consuming App evidence.
    pub app_shutdown: Option<String>,
    /// Explicit operation/setup/shutdown failures; timeout is never success.
    pub failures: Vec<String>,
}

/// Read-only actual dialog layout; positions come from the same Button.layout calls.
pub(crate) struct NativeQuestionValidationControls {
    pub(crate) presentation: NativeVisualQuestionPresentation,
    pub(crate) modal: WidgetId,
    pub(crate) ids: [WidgetId; 3],
    pub(crate) rects: [Rect; 3],
    pub(crate) card: Rect,
}

pub(super) struct NativeQuestionWindowPump {
    request: NativeQuestionWindowRequest,
    pub(super) deadline: Instant,
    pub(super) report: NativeQuestionWindowReport,
    original: Option<NativeQuestionValidationControls>,
    baseline_history: mondrian_editor_state::AuthoringHistoryDiagnostics,
    baseline_session: mondrian_editor_state::AuthoringSessionId,
    cursor: usize,
    forward: [Option<WidgetId>; 3],
    backward: [Option<WidgetId>; 3],
    activation_tabs: usize,
    pending_event: Option<UiEvent>,
    next_tick: Instant,
    done: bool,
}

impl NativeQuestionWindowPump {
    pub(super) fn new(
        request: NativeQuestionWindowRequest,
        app: &AppState,
    ) -> Result<Self, String> {
        let now = Instant::now();
        let deadline = request.deadline(now)?;
        let session =
            app.authoring.as_ref().ok_or("Question validation requires an author Session")?;
        // This only verifies the caller's existing canonical target. Actual
        // section visibility and current bank ownership are still product gates.
        app.native_visual_author_view(request.sequence, request.clip, request.effect)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            report: NativeQuestionWindowReport {
                interaction: Some(request.interaction),
                author_generation: app.project_author_generation(),
                ..Default::default()
            },
            request,
            deadline,
            original: None,
            baseline_history: session.history().diagnostics(),
            baseline_session: session.session_id(),
            cursor: 0,
            forward: [None; 3],
            backward: [None; 3],
            activation_tabs: 0,
            pending_event: None,
            next_tick: now,
            done: false,
        })
    }

    pub(super) fn fail(&mut self, error: impl Into<String>) {
        if !self.done {
            self.report.failures.push(error.into());
            self.done = true;
        }
    }

    pub(super) fn observe_surface(
        &mut self,
        host: &AppUiHost,
        original: Option<NativeVisualQuestionPresentation>,
        frame: NativeQuestionSurfaceFrame,
        window: String,
        surface: u64,
        adapter: String,
    ) {
        if self.done {
            return;
        }
        let NativeQuestionSurfaceFrame { state_before, result } = frame;
        let observation = NativeQuestionSurfaceObservation {
            window: window.clone(),
            surface_generation: surface,
            adapter: adapter.clone(),
            painted_question: original.is_some(),
            state_before: state_before.map(|state| format!("{state:?}")),
            state_after: original
                .as_ref()
                .map(|question| format!("{:?}", question.validation_state())),
            receipt: original.as_ref().map(|question| {
                let receipt = question.validation_receipt();
                format!(
                    "{:?}/{:?}/{}/{}/{}; {:?}/{}; {:?}",
                    receipt.question.owner,
                    receipt.question.origin,
                    receipt.question.id.native_owner(),
                    receipt.question.id.invocation(),
                    receipt.question.id.service(),
                    receipt.preparation,
                    receipt.provider_revision,
                    question.scope(),
                )
            }),
            frame_result: format!("{result:?}"),
            preview_observation: host.validation_question_preview_observation(),
        };
        if self.report.surface_observations.last() != Some(&observation) {
            if self.report.surface_observations.len() < 16 {
                self.report.surface_observations.push(observation);
            } else {
                self.report.surface_observations[15] = observation;
                self.report.surface_observations_replaced =
                    self.report.surface_observations_replaced.saturating_add(1);
            }
        }
        if self.original.is_some() {
            return;
        }
        let Some(presentation) = original else {
            return;
        };
        // This is called AFTER the existing finish_native_visual_question_surface.
        // A paint/Skipped/upload-first frame cannot authorize this driver.
        if presentation.validation_state() != EffectProcessorQuestionState::Presented {
            return;
        }
        let AppUiFrameResult::Presented { uploaded_resources: false, diagnostics, .. } = result
        else {
            return;
        };
        if diagnostics.has_failures() {
            self.fail(format!(
                "actual modal surface has resource failures: {diagnostics:?}"
            ));
            return;
        }
        let Some(controls) = host.root().validation_native_question_controls() else {
            self.fail("Presented Question has no actual current dialog layout");
            return;
        };
        if controls.presentation.validation_receipt() != presentation.validation_receipt()
            || presentation.author_view().sequence_id() != self.request.sequence
            || presentation.author_view().clip_id() != self.request.clip
            || presentation.author_view().effect_id() != self.request.effect
            || presentation.author_view().session_id() != self.baseline_session
            || presentation.author_view().author_generation().get() != self.report.author_generation
        {
            self.fail(
                "Presented Question does not match the original authored target/current modal",
            );
            return;
        }
        let receipt = presentation.validation_receipt();
        let mondrian_effects::EffectProcessorExecutionOrigin::SupervisedChild { process_id } =
            receipt.question.origin
        else {
            self.fail("Question validation requires an actual supervised native child");
            return;
        };
        self.report.window_id = Some(window);
        self.report.surface_generation = Some(surface);
        self.report.adapter = Some(adapter);
        self.report.modal_controls = Some([
            format!("{:?}", controls.modal),
            format!("{:?}", controls.ids[0]),
            format!("{:?}", controls.ids[1]),
            format!("{:?}", controls.ids[2]),
        ]);
        self.report.native_pid = Some(process_id.get());
        self.report.parent_owner = Some(format!("{:?}", receipt.question.owner));
        self.report.native_owner = Some(receipt.question.id.native_owner());
        self.report.invocation = Some(receipt.question.id.invocation());
        self.report.service = Some(receipt.question.id.service());
        self.report.work = Some(format!(
            "{:?} / {:?} / {}",
            presentation.scope(),
            receipt.preparation,
            receipt.provider_revision
        ));
        self.report.presented = Some(format!("{result:?}"));
        self.original = Some(controls);
    }

    fn author_unchanged(&self, app: &AppState) -> bool {
        app.authoring.as_ref().is_some_and(|session| {
            session.session_id() == self.baseline_session
                && app.project_author_generation() == self.report.author_generation
                && session.history().diagnostics() == self.baseline_history
        })
    }

    pub(super) fn next_event(
        &mut self,
        host: &AppUiHost,
        focus: Option<WidgetId>,
    ) -> Option<UiEvent> {
        if self.done {
            return None;
        }
        let now = Instant::now();
        if now >= self.deadline {
            self.fail("real Question Window/native completion deadline elapsed");
            return None;
        }
        if now < self.next_tick {
            return None;
        }
        self.next_tick = now + Duration::from_millis(8);
        if !self.author_unchanged(&host.app_state()) {
            self.fail("Question routing mutated/replaced canonical author Session or history");
            return None;
        }
        let controls = self.original.as_ref()?;
        if self.report.reply.is_some() {
            let app = host.app_state();
            if let Some(completed) = app.processor_feedback.validation_question_completion(
                controls.presentation.scope(),
                controls.presentation.validation_receipt(),
                self.request.effect,
            ) {
                self.report.completed = Some(format!("{completed:?}"));
                self.done = true;
            }
            return None;
        }
        if host.root().keyboard_focus_scope() != Some(controls.modal) {
            self.fail("original Presented Question lost the real modal scope before activation");
            return None;
        }
        // Down/up always route in separate ticks, with the ordinary action
        // drain after each. No direct child event/answer/focus manipulation.
        let event = match self.cursor {
            0 | 1 => {
                let point = Point::new(1.0, 1.0);
                if controls.card.contains(point) {
                    self.fail("actual modal card covers background probe point");
                    return None;
                }
                if self.cursor == 0 {
                    UiEvent::MouseDown {
                        position: point,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    }
                } else {
                    UiEvent::MouseUp {
                        position: point,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    }
                }
            }
            2..=13 => {
                let pair = (self.cursor - 2) / 2;
                let modifiers = if pair < 3 {
                    Modifiers::none()
                } else {
                    Modifiers::shift()
                };
                if self.cursor.is_multiple_of(2) {
                    UiEvent::KeyDown { key: KeyCode::Tab, modifiers }
                } else {
                    UiEvent::KeyUp { key: KeyCode::Tab, modifiers }
                }
            }
            _ => match self.request.interaction {
                NativeQuestionWindowInteraction::KeyboardYes => {
                    if focus != Some(controls.ids[0]) || self.activation_tabs % 2 == 1 {
                        if self.activation_tabs >= 6 {
                            self.fail("actual modal Tab traversal cannot reach Yes");
                            return None;
                        }
                        let event = if self.activation_tabs.is_multiple_of(2) {
                            UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::none() }
                        } else {
                            UiEvent::KeyUp { key: KeyCode::Tab, modifiers: Modifiers::none() }
                        };
                        self.activation_tabs += 1;
                        self.pending_event = Some(event.clone());
                        return Some(event);
                    }
                    if self.cursor == 14 {
                        UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() }
                    } else {
                        UiEvent::KeyUp { key: KeyCode::Enter, modifiers: Modifiers::none() }
                    }
                }
                NativeQuestionWindowInteraction::PointerNo => {
                    let rect = controls.rects[1];
                    let point = Point::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
                    if !rect.contains(point) || !controls.card.contains(point) {
                        self.fail("actual No Button layout has no visible hit point");
                        return None;
                    }
                    if self.cursor == 14 {
                        UiEvent::MouseDown {
                            position: point,
                            button: MouseButton::Left,
                            modifiers: Modifiers::none(),
                        }
                    } else {
                        UiEvent::MouseUp {
                            position: point,
                            button: MouseButton::Left,
                            modifiers: Modifiers::none(),
                        }
                    }
                }
                NativeQuestionWindowInteraction::EscapeDismiss => {
                    UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() }
                }
            },
        };
        self.pending_event = Some(event.clone());
        Some(event)
    }

    pub(super) fn after_event(&mut self, host: &AppUiHost, focus: Option<WidgetId>) {
        let Some(event) = self.pending_event.take() else {
            return;
        };
        self.report.events.push(format!("{event:?}; focused={focus:?}"));
        if self.report.events.len() > 24 {
            self.fail("bounded Question input script exhausted");
            return;
        }
        let Some(controls) = self.original.as_ref() else {
            self.fail("routed event has no original Presented Question");
            return;
        };
        if !self.author_unchanged(&host.app_state()) {
            self.fail("modal input changed author state/history");
            return;
        }
        if matches!(event, UiEvent::KeyDown { key: KeyCode::Tab, .. }) {
            if !focus.is_some_and(|id| controls.ids.contains(&id)) {
                self.fail("Tab/ShiftTab escaped the real Question modal children");
                return;
            }
            if (2..=13).contains(&self.cursor) {
                let pair = (self.cursor - 2) / 2;
                if pair < 3 {
                    self.forward[pair] = focus;
                } else {
                    self.backward[pair - 3] = focus;
                }
            }
        }
        if self.cursor == 13
            && !controls
                .ids
                .iter()
                .all(|id| self.forward.contains(&Some(*id)) && self.backward.contains(&Some(*id)))
        {
            self.fail("forward/backward Tab did not visit all three actual modal buttons");
            return;
        }
        let state = controls.presentation.validation_state();
        if let EffectProcessorQuestionState::Answered(reply) = state {
            if self.cursor < 14 || reply != self.request.interaction.expected() {
                self.fail(format!(
                    "unexpected actual routed Question reply: {state:?}"
                ));
                return;
            }
            self.report.reply = Some(format!("{reply:?}"));
        } else if state != EffectProcessorQuestionState::Presented {
            self.fail(format!(
                "original Question disappeared before its routed answer: {state:?}"
            ));
            return;
        }
        if !matches!(
            event,
            UiEvent::KeyDown { key: KeyCode::Tab, .. } | UiEvent::KeyUp { key: KeyCode::Tab, .. }
        ) || self.cursor < 14
        {
            self.cursor += 1;
        }
    }

    pub(super) fn finished(&self) -> bool {
        self.done
    }
    pub(super) fn wake(&self) -> Instant {
        self.next_tick.min(self.deadline)
    }
}

#[cfg(test)]
mod question_window_observer_contract_tests {
    use super::*;
    use crate::app::preview_native_gui::question_contract_tests::fixture;
    use mondrian_effects::EffectProcessorQuestionView;

    #[test]
    fn question_window_observer_keeps_actual_terminal_reply_without_reviving_service() {
        let fixture = fixture();
        let generation = fixture.session.author_generation();
        let history = fixture.session.history().diagnostics();
        // Value-layer setup only. This unit test has no Window surface claim.
        assert!(fixture.presentation.presented());
        assert!(fixture.presentation.answer(EffectProcessorQuestionReply::Yes));
        fixture.service.finish_question(EffectProcessorQuestionView {
            preparation: fixture.preparation,
            provider_revision: 7,
            question: &fixture.receipt,
        });
        assert_eq!(
            fixture.presentation.validation_state(),
            EffectProcessorQuestionState::Answered(EffectProcessorQuestionReply::Yes)
        );
        assert!(!fixture.presentation.is_current());
        assert!(!fixture.presentation.presented());
        assert!(!fixture.presentation.answer(EffectProcessorQuestionReply::No));
        assert_eq!(fixture.session.author_generation(), generation);
        assert_eq!(fixture.session.history().diagnostics(), history);
    }

    #[test]
    fn question_window_observer_points_are_actual_button_layout_and_ids() {
        use crate::app_ui::localization::AppUiLocale;
        use crate::app_ui::native_visual_question_dialog::NativeVisualQuestionDialog;
        let fixture = fixture();
        let mut dialog =
            NativeVisualQuestionDialog::new(fixture.presentation.clone(), AppUiLocale::ZhCn);
        dialog.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        let controls = dialog.validation_controls().expect("original current actual modal");
        assert_eq!(controls.modal, dialog.id());
        for (index, rect) in controls.rects.into_iter().enumerate() {
            let button = dialog.child(index + 2).expect("actual local Button child");
            let point = Point::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
            assert_eq!(button.id(), controls.ids[index]);
            assert!(button.can_focus());
            assert!(!button.accessibility().expect("actual accessible Button").state.disabled);
            assert!(controls.card.contains(point) && button.hit_test(point));
        }
        assert!(!fixture.session.history().can_undo());
    }
}
