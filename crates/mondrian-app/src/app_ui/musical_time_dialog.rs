//! A shell-local text draft; only Apply emits the existing Sequence transaction.
use crate::app::product_action::SequenceSetMusicalTimeReferencePayload;
use crate::app::ui_actions::{
    app_shell_close_modal_action, app_shell_confirm_musical_time_action,
    app_shell_musical_time_draft_changed_action, MusicalTimeDraftUpdate,
};
use crate::app_ui::localization::{AppUiLocale, Localizer};
use mondrian_core::{MusicalTimeReference, MusicalTimeSignature, SequenceId, TimelineTime};
use mondrian_timeline::Sequence;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::{Button, Checkbox, DialogSurface, Label, TextInput};

#[derive(Clone)]
pub(crate) struct MusicalTimeDraft {
    sequence_id: SequenceId,
    enabled: bool,
    tempo: String,
    origin: String,
    signature: String,
}

impl MusicalTimeDraft {
    pub(crate) fn from_sequence(sequence: &Sequence) -> Self {
        let reference = sequence.audio_program.musical_time_reference;
        Self {
            sequence_id: sequence.id,
            enabled: reference.is_some(),
            tempo: reference.map_or_else(
                || "120".into(),
                |r| {
                    let duration = r.quarter_note_duration();
                    format_ratio(
                        60 * i128::from(duration.denominator()),
                        i128::from(duration.numerator()),
                    )
                },
            ),
            origin: reference.map_or_else(
                || "0".into(),
                |r| {
                    let origin = r.quarter_note_zero();
                    format_ratio(
                        i128::from(origin.numerator()),
                        i128::from(origin.denominator()),
                    )
                },
            ),
            signature: reference
                .and_then(MusicalTimeReference::time_signature)
                .map_or_else(String::new, |s| {
                    format!("{}/{}", s.numerator(), s.denominator())
                }),
        }
    }

    pub(crate) fn into_payload(
        self,
    ) -> Result<SequenceSetMusicalTimeReferencePayload, &'static str> {
        let reference = if self.enabled {
            let (n, d) =
                parse_ratio(&self.tempo).filter(|(n, _)| *n > 0).ok_or("music-invalid-tempo")?;
            // Reduce before multiplying: all valid Core references, including tiny
            // durations whose BPM exceeds i64, round-trip without a float or clamp.
            let divisor = gcd(n as u128, 60) as i128;
            let duration = d
                .checked_mul(60 / divisor)
                .and_then(|value| to_time(value, n / divisor))
                .ok_or("music-invalid-tempo")?;
            let origin = parse_ratio(&self.origin)
                .and_then(|(n, d)| to_time(n, d))
                .ok_or("music-invalid-origin")?;
            let signature = if self.signature.trim().is_empty() {
                None
            } else {
                let (n, d) =
                    self.signature.trim().split_once('/').ok_or("music-invalid-signature")?;
                Some(
                    MusicalTimeSignature::new(
                        n.trim().parse().map_err(|_| "music-invalid-signature")?,
                        d.trim().parse().map_err(|_| "music-invalid-signature")?,
                    )
                    .map_err(|_| "music-invalid-signature")?,
                )
            };
            Some(
                MusicalTimeReference::new(origin, duration)
                    .map_err(|_| "music-invalid-tempo")?
                    .with_time_signature(signature),
            )
        } else {
            None
        };
        Ok(SequenceSetMusicalTimeReferencePayload { sequence_id: self.sequence_id, reference })
    }
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn format_ratio(n: i128, d: i128) -> String {
    let divisor = gcd(n.unsigned_abs(), d as u128) as i128;
    let (n, d) = (n / divisor, d / divisor);
    if d == 1 {
        n.to_string()
    } else {
        format!("{n}/{d}")
    }
}

fn to_time(n: i128, d: i128) -> Option<TimelineTime> {
    let divisor = gcd(n.unsigned_abs(), d as u128) as i128;
    TimelineTime::new(
        i64::try_from(n / divisor).ok()?,
        i64::try_from(d / divisor).ok()?,
    )
    .ok()
}

/// Deliberately finite decimal/fraction grammar. Never reinterpret NaN, exponent,
/// overflow or an incomplete edit as zero; invalid text stays in the local draft.
fn parse_ratio(text: &str) -> Option<(i128, i128)> {
    let text = text.trim();
    if text.is_empty() || text.len() > 128 {
        return None;
    }
    let (n, d) = if let Some((n, d)) = text.split_once('/') {
        (
            n.trim().parse::<i128>().ok()?,
            d.trim().parse::<i128>().ok()?,
        )
    } else {
        let (sign, digits) = if let Some(rest) = text.strip_prefix('-') {
            ("-", rest)
        } else {
            ("", text.strip_prefix('+').unwrap_or(text))
        };
        let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
        if whole.is_empty() && fraction.is_empty()
            || !whole.bytes().chain(fraction.bytes()).all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let fraction = fraction.trim_end_matches('0');
        let d = 10_i128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
        let n = format!(
            "{sign}{}{fraction}",
            if whole.is_empty() { "0" } else { whole }
        )
        .parse::<i128>()
        .ok()?;
        (n, d)
    };
    if d <= 0 {
        return None;
    }
    let divisor = gcd(n.unsigned_abs(), d as u128) as i128;
    Some((n / divisor, d / divisor))
}

pub struct MusicalTimeDialog {
    id: WidgetId,
    draft: MusicalTimeDraft,
    localizer: Localizer,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    title: Label,
    description: Label,
    enabled: Checkbox,
    labels: [Label; 3],
    inputs: [TextInput; 3],
    error: Label,
    cancel: Button,
    apply: Button,
}

impl MusicalTimeDialog {
    pub(crate) fn new(draft: MusicalTimeDraft, locale: AppUiLocale) -> Self {
        let localizer = Localizer::new(locale).expect("bundled UI catalogs must be valid");
        let keys = ["music-tempo", "music-origin", "music-signature"];
        let values = [&draft.tempo, &draft.origin, &draft.signature];
        let updates: [fn(String) -> MusicalTimeDraftUpdate; 3] = [
            MusicalTimeDraftUpdate::Tempo,
            MusicalTimeDraftUpdate::Origin,
            MusicalTimeDraftUpdate::Signature,
        ];
        let inputs = std::array::from_fn(|i| {
            TextInput::new(localizer.text(keys[i]))
                .with_text(values[i])
                .on_change(move |text| {
                    app_shell_musical_time_draft_changed_action(updates[i](text.into()))
                })
        });
        let labels = keys.map(|key| Label::new(localizer.text(key)).muted().with_padding(0.0, 0.0));
        let mut dialog = Self {
            id: WidgetId::new(),
            draft,
            surface: DialogSurface::new(Size::new(480.0, 480.0), Size::new(620.0, 500.0))
                .with_content_padding(24.0),
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            title: Label::new(localizer.text("music-title"))
                .popover_foreground()
                .with_font_size(20.0)
                .with_padding(0.0, 0.0),
            description: Label::new(localizer.text("music-description"))
                .muted()
                .wrapped()
                .with_padding(0.0, 0.0),
            enabled: Checkbox::new(localizer.text("music-enabled"), false).on_change(|enabled| {
                app_shell_musical_time_draft_changed_action(MusicalTimeDraftUpdate::Enabled(
                    enabled,
                ))
            }),
            labels,
            inputs,
            error: Label::new("")
                .with_semantic_color(mondrian_ui_widgets::label::LabelColor::Destructive)
                .wrapped()
                .with_padding(0.0, 0.0),
            cancel: Button::new(localizer.text("sequence-cancel"))
                .on_click(app_shell_close_modal_action()),
            apply: Button::new(localizer.text("sequence-apply"))
                .on_click(app_shell_confirm_musical_time_action()),
            localizer,
        };
        dialog.synchronize_controls();
        dialog
    }

    pub(crate) fn payload(&self) -> Option<SequenceSetMusicalTimeReferencePayload> {
        if self.inputs.iter().any(TextInput::is_composing) {
            return None;
        }
        self.draft.clone().into_payload().ok()
    }

    pub(crate) fn apply_update(&mut self, update: MusicalTimeDraftUpdate) {
        match update {
            MusicalTimeDraftUpdate::Enabled(enabled) => self.draft.enabled = enabled,
            MusicalTimeDraftUpdate::Tempo(value) => self.draft.tempo = value,
            MusicalTimeDraftUpdate::Origin(value) => self.draft.origin = value,
            MusicalTimeDraftUpdate::Signature(value) => self.draft.signature = value,
        }
        self.synchronize_controls();
    }

    fn synchronize_controls(&mut self) {
        self.enabled.set_checked(self.draft.enabled);
        for (input, value) in self.inputs.iter_mut().zip([
            &self.draft.tempo,
            &self.draft.origin,
            &self.draft.signature,
        ]) {
            input.set_enabled(self.draft.enabled);
            if input.text() != value {
                input.set_text(value.clone());
            }
        }
        let error = self.draft.clone().into_payload().err();
        self.apply.set_enabled(error.is_none());
        self.error
            .set_text(error.map_or_else(String::new, |key| self.localizer.text(key)));
    }
}

impl Widget for MusicalTimeDialog {
    fn id(&self) -> WidgetId {
        self.id
    }
    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        self.surface.preferred_size()
    }
    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.card = self.surface.card_rect(bounds);
        let content = self.surface.content_rect(self.card);
        self.title.layout(Rect::new(content.x, content.y, content.width, 30.0));
        self.description
            .layout(Rect::new(content.x, content.y + 38.0, content.width, 64.0));
        self.enabled
            .layout(Rect::new(content.x, content.y + 110.0, content.width, 28.0));
        for (i, (label, input)) in self.labels.iter_mut().zip(&mut self.inputs).enumerate() {
            let y = content.y + 152.0 + i as f32 * 62.0;
            label.layout(Rect::new(content.x, y, content.width, 20.0));
            input.layout(Rect::new(content.x, y + 22.0, content.width, 32.0));
        }
        let y = content.y + content.height - 32.0;
        self.error.layout(Rect::new(
            content.x,
            content.y + 340.0,
            content.width,
            (y - content.y - 348.0).max(0.0),
        ));
        self.cancel.layout(Rect::new(content.x + content.width - 208.0, y, 100.0, 32.0));
        self.apply.layout(Rect::new(content.x + content.width - 100.0, y, 100.0, 32.0));
    }
    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if self.inputs.iter().any(TextInput::is_composing) {
            if matches!(event, UiEvent::MouseDown { position, .. } | UiEvent::MouseUp { position, .. }
                if self.apply.hit_test(*position))
            {
                return EventResult::Handled;
            }
            for input in &mut self.inputs {
                if input.is_composing() && input.event(event, ctx) == EventResult::Handled {
                    return EventResult::Handled;
                }
            }
        }
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                (ctx.dispatch)(app_shell_close_modal_action());
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Enter, modifiers }
                if *modifiers == Modifiers::none() =>
            {
                if self.payload().is_some() {
                    (ctx.dispatch)(app_shell_confirm_musical_time_action());
                }
                return EventResult::Handled;
            }
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                return EventResult::Handled
            }
            _ => {}
        }
        for i in 0..self.child_count() {
            if self
                .child_mut(i)
                .is_some_and(|child| child.event(event, ctx) == EventResult::Handled)
            {
                return EventResult::Handled;
            }
        }
        EventResult::Ignored
    }
    fn paint(&self, ctx: &mut PaintContext) {
        self.surface.paint(self.bounds, self.card, ctx);
        for i in 0..self.child_count() {
            if let Some(child) = self.child(i) {
                child.paint(ctx);
            }
        }
    }
    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
    fn child_count(&self) -> usize {
        12
    }
    fn child(&self, i: usize) -> Option<&dyn Widget> {
        match i {
            0 => Some(&self.title),
            1 => Some(&self.description),
            2 => Some(&self.enabled),
            3..=5 => Some(&self.labels[i - 3]),
            6..=8 => Some(&self.inputs[i - 6]),
            9 => Some(&self.error),
            10 => Some(&self.cancel),
            11 => Some(&self.apply),
            _ => None,
        }
    }
    fn child_mut(&mut self, i: usize) -> Option<&mut dyn Widget> {
        match i {
            0 => Some(&mut self.title),
            1 => Some(&mut self.description),
            2 => Some(&mut self.enabled),
            3..=5 => Some(&mut self.labels[i - 3]),
            6..=8 => Some(&mut self.inputs[i - 6]),
            9 => Some(&mut self.error),
            10 => Some(&mut self.cancel),
            11 => Some(&mut self.apply),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
