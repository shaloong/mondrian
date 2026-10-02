//! Typed, cancellable Inspector resource drafts. Only confirmation emits an author action.

use crate::app::ui_actions::{
    clip_write_parameter_values_action, visual_effect_set_parameter_value_action,
    visual_mask_set_parameter_value_action, ClipWriteParameterValuesPayload, ParameterValueWrite,
    VisualEffectSetParameterValuePayload, VisualMaskSetParameterValuePayload, APP_SHELL_NAMESPACE,
};
use crate::app_ui::localization::{AppUiLocale, Localizer};
use crate::app_ui::panels::{InspectorEffectPropertyModel, InspectorPanelModel};
use mondrian_core::{
    AnimationParameterAddress, AssetId, ClipId, EffectId, MaskId, ParameterResourceKind,
    ParameterResourceReference, ParameterSchema, PropertyValue, ResourcePathKind, SequenceId,
};
use mondrian_editor_state::{Action, AuthoringSessionId};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_widgets::{Button, DialogSurface, Dropdown, Label, MenuItem, TextInput};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Outcome of routing a shell-local resource editor event.
pub(crate) enum ResourceActionOutcome {
    Unhandled,
    Handled(Option<Action>),
}

pub(crate) const RESOURCE_EDITOR: &str = "resource_editor";

/// Stable owner address shared by Inspector widgets and resource drafts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum InspectorPropertyTarget {
    Clip {
        parameter: AnimationParameterAddress,
    },
    Effect {
        effect_id: EffectId,
        parameter: AnimationParameterAddress,
    },
    Mask {
        mask_id: MaskId,
        parameter: AnimationParameterAddress,
    },
}
impl InspectorPropertyTarget {
    pub(crate) fn write_action(
        &self,
        clip_id: ClipId,
        write: impl FnOnce(AnimationParameterAddress) -> ParameterValueWrite,
    ) -> Action {
        match self {
            Self::Clip { parameter } => {
                clip_write_parameter_values_action(ClipWriteParameterValuesPayload {
                    clip_id,
                    writes: vec![write(parameter.clone())],
                })
            }
            Self::Effect { effect_id, parameter } => {
                visual_effect_set_parameter_value_action(VisualEffectSetParameterValuePayload {
                    clip_id,
                    effect_id: *effect_id,
                    write: write(parameter.clone()),
                })
            }
            Self::Mask { mask_id, parameter } => {
                visual_mask_set_parameter_value_action(VisualMaskSetParameterValuePayload {
                    clip_id,
                    mask_id: *mask_id,
                    write: write(parameter.clone()),
                })
            }
        }
    }
    pub(crate) fn property<'a>(
        &self,
        clip_id: ClipId,
        model: &'a InspectorPanelModel,
    ) -> Option<&'a InspectorEffectPropertyModel> {
        if !model.is_editable || model.selected_clip?.clip_id != clip_id {
            return None;
        }
        let (parameter, properties) = match self {
            Self::Clip { parameter } => (parameter, &model.clip_properties),
            Self::Effect { effect_id, parameter } => (
                parameter,
                &model.effects.iter().find(|effect| effect.effect_id == *effect_id)?.properties,
            ),
            Self::Mask { mask_id, parameter } => (
                parameter,
                &model
                    .masks
                    .iter()
                    .find(|mask| mask.mask_id == *mask_id && !mask.locked)?
                    .properties,
            ),
        };
        properties.iter().find(|property| property.address == *parameter)
    }
}

/// A unique draft identity prevents delayed events from changing a newer dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ResourceDialogId(uuid::Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ResourceKind {
    Unbound,
    ExternalFile,
    ProjectAsset,
    Uri,
}
impl ResourceKind {
    fn message(self) -> &'static str {
        match self {
            Self::Unbound => "resource-unbound",
            Self::ExternalFile => "resource-file",
            Self::ProjectAsset => "resource-project-asset",
            Self::Uri => "resource-uri",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum ResourceUpdate {
    Kind(ResourceKind),
    Text(String),
    Asset(AssetId),
    Search(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum ResourceEditorAction {
    Open {
        clip_id: ClipId,
        target: InspectorPropertyTarget,
    },
    Update {
        id: ResourceDialogId,
        update: ResourceUpdate,
    },
    Browse {
        id: ResourceDialogId,
        folder: bool,
    },
    Confirm {
        id: ResourceDialogId,
    },
    Cancel {
        id: ResourceDialogId,
    },
}
impl ResourceEditorAction {
    pub(crate) fn action(self) -> Action {
        // These owned fields always have a serializable representation.
        Action::Custom {
            namespace: APP_SHELL_NAMESPACE.to_owned(),
            name: RESOURCE_EDITOR.to_owned(),
            payload: serde_json::to_value(self).expect("resource UI action is serializable"),
        }
    }
    pub(crate) fn id(&self) -> Option<ResourceDialogId> {
        match self {
            Self::Open { .. } => None,
            Self::Update { id, .. }
            | Self::Browse { id, .. }
            | Self::Confirm { id }
            | Self::Cancel { id } => Some(*id),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResourceAssetChoice {
    pub(crate) id: AssetId,
    pub(crate) label: String,
}

#[derive(Clone)]
pub(crate) struct ResourceDraft {
    pub(crate) id: ResourceDialogId,
    pub(crate) session: Option<AuthoringSessionId>,
    pub(crate) sequence: Option<SequenceId>,
    pub(crate) clip_id: ClipId,
    pub(crate) target: InspectorPropertyTarget,
    pub(crate) original: ParameterResourceReference,
    pub(crate) schema: ParameterSchema,
    kind: ResourceKind,
    file: Option<PathBuf>,
    uri: String,
    asset: Option<AssetId>,
    search: String,
}
impl ResourceDraft {
    pub(crate) fn new(
        clip_id: ClipId,
        target: InspectorPropertyTarget,
        original: ParameterResourceReference,
        schema: ParameterSchema,
        session: Option<AuthoringSessionId>,
        sequence: Option<SequenceId>,
    ) -> Self {
        let (kind, file, uri, asset) = match &original {
            ParameterResourceReference::Unbound => {
                (ResourceKind::Unbound, None, String::new(), None)
            }
            ParameterResourceReference::ExternalFile { path } => (
                ResourceKind::ExternalFile,
                Some(path.clone()),
                String::new(),
                None,
            ),
            ParameterResourceReference::Uri { uri } => (ResourceKind::Uri, None, uri.clone(), None),
            ParameterResourceReference::ProjectAsset { asset_id } => (
                ResourceKind::ProjectAsset,
                None,
                String::new(),
                Some(*asset_id),
            ),
        };
        Self {
            id: ResourceDialogId(uuid::Uuid::new_v4()),
            session,
            sequence,
            clip_id,
            target,
            original,
            schema,
            kind,
            file,
            uri,
            asset,
            search: String::new(),
        }
    }
    fn allows_kind(&self, kind: ResourceKind) -> bool {
        let binding = match kind {
            ResourceKind::Unbound => return true,
            ResourceKind::ExternalFile => ParameterResourceKind::ExternalFile,
            ResourceKind::ProjectAsset => ParameterResourceKind::ProjectAsset,
            ResourceKind::Uri => ParameterResourceKind::Uri,
        };
        self.schema
            .resource
            .as_ref()
            .is_some_and(|contract| contract.allowed_kinds.contains(&binding))
    }

    fn kind_message(&self, kind: ResourceKind) -> &'static str {
        if kind == ResourceKind::ExternalFile {
            match self.schema.resource.as_ref().and_then(|contract| contract.path_kind) {
                Some(ResourcePathKind::File) => return "resource-file-only",
                Some(ResourcePathKind::Directory) => return "resource-folder-only",
                Some(ResourcePathKind::FileOrDirectory) | None => {}
            }
        }
        kind.message()
    }

    pub(crate) fn confirmed_value(
        &self,
    ) -> Result<Option<ParameterResourceReference>, &'static str> {
        let unchanged = match (self.kind, &self.original) {
            (ResourceKind::Unbound, ParameterResourceReference::Unbound) => true,
            (ResourceKind::ExternalFile, ParameterResourceReference::ExternalFile { path }) => {
                self.file.as_ref() == Some(path)
            }
            (ResourceKind::ProjectAsset, ParameterResourceReference::ProjectAsset { asset_id }) => {
                self.asset == Some(*asset_id)
            }
            (ResourceKind::Uri, ParameterResourceReference::Uri { uri }) => &self.uri == uri,
            _ => false,
        };
        if unchanged {
            // No new author intent exists to validate, resolve or allocate.
            return Ok(None);
        }
        self.value().map(Some)
    }
    pub(crate) fn allows_browse(&self, folder: bool) -> bool {
        self.kind == ResourceKind::ExternalFile
            && self
                .schema
                .resource
                .as_ref()
                .and_then(|contract| contract.path_kind)
                .is_some_and(|kind| match kind {
                    ResourcePathKind::File => !folder,
                    ResourcePathKind::Directory => folder,
                    ResourcePathKind::FileOrDirectory => true,
                })
    }
    fn text(&self) -> String {
        match self.kind {
            ResourceKind::ExternalFile => {
                self.file.as_ref().map_or_else(String::new, |path| path.display().to_string())
            }
            ResourceKind::Uri => self.uri.clone(),
            ResourceKind::ProjectAsset => self.search.clone(),
            ResourceKind::Unbound => String::new(),
        }
    }
    fn update(&mut self, update: ResourceUpdate) {
        match update {
            ResourceUpdate::Kind(kind) if self.allows_kind(kind) => self.kind = kind,
            ResourceUpdate::Kind(_) => {}
            ResourceUpdate::Text(text) => match self.kind {
                ResourceKind::ExternalFile => {
                    self.file = (!text.is_empty()).then(|| PathBuf::from(text))
                }
                ResourceKind::Uri => self.uri = text,
                _ => {}
            },
            ResourceUpdate::Asset(asset) => self.asset = Some(asset),
            ResourceUpdate::Search(search) => self.search = search,
        }
    }
    pub(crate) fn value(&self) -> Result<ParameterResourceReference, &'static str> {
        let value = match self.kind {
            ResourceKind::Unbound => Ok(ParameterResourceReference::Unbound),
            ResourceKind::ExternalFile => self
                .file
                .as_ref()
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| ParameterResourceReference::ExternalFile { path: path.clone() })
                .ok_or("resource-path-required"),
            ResourceKind::ProjectAsset => self
                .asset
                .map(|asset_id| ParameterResourceReference::ProjectAsset { asset_id })
                .ok_or("resource-asset-required"),
            ResourceKind::Uri => {
                if self.uri.is_empty()
                    || self.uri.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\')
                    || self.uri.as_bytes().iter().enumerate().any(|(index, byte)| {
                        *byte == b'%'
                            && (self
                                .uri
                                .as_bytes()
                                .get(index + 1)
                                .is_none_or(|b| !b.is_ascii_hexdigit())
                                || self
                                    .uri
                                    .as_bytes()
                                    .get(index + 2)
                                    .is_none_or(|b| !b.is_ascii_hexdigit()))
                    })
                    || url::Url::parse(&self.uri).is_err()
                {
                    return Err("resource-uri-invalid");
                }
                Ok(ParameterResourceReference::Uri { uri: self.uri.clone() })
            }
        }?;
        if self.schema.resource.as_ref().is_none_or(|contract| !contract.admits(&value)) {
            return Err("resource-not-admitted");
        }
        Ok(value)
    }
    /// Validate against author authority without rebuilding Inspector curves.
    pub(crate) fn matches_state(&self, state: &crate::app::AppState) -> bool {
        if self.session != state.authoring_session_id()
            || self.sequence != state.active_sequence_id()
            || state
                .primary_selected_clip()
                .is_none_or(|selection| selection.clip_id != self.clip_id)
        {
            return false;
        }
        let Some(sequence) = state.active_sequence() else {
            return false;
        };
        if sequence
            .clip_track_location(self.clip_id)
            .is_none_or(|location| location.is_locked)
        {
            return false;
        }
        let Some(clip) = sequence.find_clip(self.clip_id) else {
            return false;
        };
        let matches = |bag: &mondrian_core::automation::PropertyBag, parameter| {
            bag.property_by_address(parameter).is_some_and(|(_, property)| {
                property.descriptor.schema == self.schema
                    && matches!(property.static_value(), PropertyValue::Resource(reference) if reference == &self.original)
            })
        };
        match &self.target {
            InspectorPropertyTarget::Clip { parameter } => clip
                .content
                .basic_title()
                .is_some_and(|title| matches(&title.property_bag(), parameter)),
            InspectorPropertyTarget::Effect { effect_id, parameter } => clip
                .effects
                .iter()
                .find(|effect| effect.id == *effect_id)
                .is_some_and(|effect| matches(&effect.properties, parameter)),
            InspectorPropertyTarget::Mask { mask_id, parameter } => clip
                .mask(*mask_id)
                .is_some_and(|mask| !mask.locked && matches(&mask.properties, parameter)),
        }
    }
    pub(crate) fn matches_model(&self, model: &InspectorPanelModel) -> bool {
        self.target.property(self.clip_id, model).is_some_and(|property| {
            property.schema == self.schema
                && matches!(&property.value, PropertyValue::Resource(reference) if reference == &self.original)
        })
    }
    pub(crate) fn author_action(&self, value: ParameterResourceReference) -> Option<Action> {
        if value == self.original {
            return None;
        }
        Some(
            self.target.write_action(self.clip_id, |parameter| ParameterValueWrite::Value {
                parameter,
                value: PropertyValue::Resource(value),
            }),
        )
    }
}

/// Modal editing a resource reference without altering project or library state.
pub struct ResourceDialog {
    id: WidgetId,
    pub(crate) draft: ResourceDraft,
    assets: Vec<ResourceAssetChoice>,
    asset_search_keys: Vec<String>,
    error_key: Option<String>,
    localizer: Localizer,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    title: Label,
    kind: Dropdown,
    input: TextInput,
    choices: Dropdown,
    file: Button,
    folder: Button,
    error: Label,
    apply: Button,
    cancel: Button,
}
impl ResourceDialog {
    pub(crate) fn new(
        draft: ResourceDraft,
        label: String,
        assets: Vec<ResourceAssetChoice>,
        locale: AppUiLocale,
    ) -> Self {
        let localizer = Localizer::new(locale).expect("bundled UI catalogs must be valid");
        let id = draft.id;
        let theme = mondrian_ui_theme::current_theme();
        let asset_search_keys = assets.iter().map(|asset| asset.label.to_lowercase()).collect();
        let mut dialog = Self {
            id: WidgetId::new(),
            draft,
            assets,
            asset_search_keys,
            error_key: None,
            surface: DialogSurface::new(Size::new(320.0, 340.0), Size::new(560.0, 340.0))
                .with_content_padding(2.0 * theme.spacing.md),
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            title: Label::new(label)
                .popover_foreground()
                .with_font_size(theme.typography.heading_h3.font_size)
                .with_padding(0.0, 0.0),
            kind: Dropdown::new("", Vec::new()),
            input: TextInput::new(""),
            choices: Dropdown::new("", Vec::new()),
            file: Button::new(localizer.text("resource-browse-file")).on_click(Some(
                ResourceEditorAction::Browse { id, folder: false }.action(),
            )),
            folder: Button::new(localizer.text("resource-browse-folder")).on_click(Some(
                ResourceEditorAction::Browse { id, folder: true }.action(),
            )),
            error: Label::new("").wrapped().muted(),
            apply: Button::new(localizer.text("resource-apply"))
                .on_click(Some(ResourceEditorAction::Confirm { id }.action())),
            cancel: Button::new(localizer.text("resource-cancel"))
                .on_click(Some(ResourceEditorAction::Cancel { id }.action())),
            localizer,
        };
        drop(theme);
        dialog.rebuild_controls(true);
        dialog
    }
    fn rebuild_controls(&mut self, replace_input: bool) {
        let id = self.draft.id;
        let items = [
            ResourceKind::Unbound,
            ResourceKind::ExternalFile,
            ResourceKind::ProjectAsset,
            ResourceKind::Uri,
        ]
        .into_iter()
        .filter(|kind| self.draft.allows_kind(*kind))
        .map(|kind| {
            MenuItem::new(
                self.localizer.text(self.draft.kind_message(kind)),
                ResourceEditorAction::Update { id, update: ResourceUpdate::Kind(kind) }.action(),
            )
            .checked(self.draft.kind == kind)
        })
        .collect();
        self.kind = Dropdown::new(
            self.localizer.text(self.draft.kind_message(self.draft.kind)),
            items,
        );
        if replace_input {
            let asset = self.draft.kind == ResourceKind::ProjectAsset;
            self.input = TextInput::new(self.localizer.text(if asset {
                "resource-search-assets"
            } else {
                "resource-value"
            }))
            .with_text(self.draft.text())
            .enabled(self.draft.kind != ResourceKind::Unbound)
            .on_change(move |text| {
                ResourceEditorAction::Update {
                    id,
                    update: if asset {
                        ResourceUpdate::Search(text.to_owned())
                    } else {
                        ResourceUpdate::Text(text.to_owned())
                    },
                }
                .action()
            });
        }
        let query = self.draft.search.to_lowercase();
        let items: Vec<_> = self
            .assets
            .iter()
            .enumerate()
            .filter(|(index, _)| self.asset_search_keys[*index].contains(&query))
            .map(|(_, asset)| {
                MenuItem::new(
                    asset.label.clone(),
                    ResourceEditorAction::Update { id, update: ResourceUpdate::Asset(asset.id) }
                        .action(),
                )
                .checked(self.draft.asset == Some(asset.id))
            })
            .collect();
        if self.asset_mode() && !query.is_empty() && items.is_empty() {
            self.set_error("resource-no-assets-match");
        }
        let label =
            self.assets.iter().find(|asset| Some(asset.id) == self.draft.asset).map_or_else(
                || {
                    self.localizer.text(if self.draft.asset.is_some() {
                        "resource-unavailable-asset"
                    } else {
                        "resource-select-asset"
                    })
                },
                |asset| asset.label.clone(),
            );
        self.choices = Dropdown::new(label, items).with_max_visible_items(8);
        self.layout(self.bounds);
    }
    pub(crate) fn apply_update(&mut self, update: ResourceUpdate) {
        let replace_input = matches!(update, ResourceUpdate::Kind(_));
        let search = matches!(update, ResourceUpdate::Search(_) | ResourceUpdate::Asset(_));
        self.draft.update(update);
        self.error_key = None;
        self.error.set_text(String::new());
        if replace_input || search {
            self.rebuild_controls(replace_input);
        }
    }
    pub(crate) fn selected_path(&mut self, path: PathBuf) {
        self.draft.file = Some(path);
        self.error_key = None;
        self.rebuild_controls(true);
        self.error.set_text(String::new());
    }
    pub(crate) fn set_error(&mut self, key: &str) {
        self.error_key = Some(key.to_owned());
        self.error.set_text(self.localizer.text(key));
    }

    pub(crate) fn set_locale(&mut self, locale: AppUiLocale, label: String) {
        self.title.set_text(label);
        if self.localizer.locale() == locale {
            return;
        }
        self.localizer = Localizer::new(locale).expect("bundled UI catalogs must be valid");
        self.file.set_label(self.localizer.text("resource-browse-file"));
        self.folder.set_label(self.localizer.text("resource-browse-folder"));
        self.apply.set_label(self.localizer.text("resource-apply"));
        self.cancel.set_label(self.localizer.text("resource-cancel"));
        self.input.set_placeholder(self.localizer.text(if self.asset_mode() {
            "resource-search-assets"
        } else {
            "resource-value"
        }));
        self.rebuild_controls(false);
        self.error.set_text(
            self.error_key
                .as_deref()
                .map_or_else(String::new, |key| self.localizer.text(key)),
        );
    }
    pub(crate) fn is_composing(&self) -> bool {
        self.input.is_composing()
    }
    fn asset_mode(&self) -> bool {
        self.draft.kind == ResourceKind::ProjectAsset
    }
}
impl Widget for ResourceDialog {
    fn id(&self) -> WidgetId {
        self.id
    }
    fn measure(&self, _: LayoutConstraint) -> Size {
        self.surface.preferred_size()
    }
    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.card = self.surface.card_rect(bounds);
        let area = self.surface.content_rect(self.card);
        let theme = mondrian_ui_theme::current_theme();
        let height = theme.spacing.interact_height;
        let gap = theme.spacing.md;
        let step = height + gap;
        self.title.layout(Rect::new(area.x, area.y, area.width, height));
        self.kind.layout(Rect::new(area.x, area.y + step, area.width, height));
        self.input.layout(Rect::new(area.x, area.y + 2.0 * step, area.width, height));
        self.choices.layout(Rect::new(area.x, area.y + 3.0 * step, area.width, height));
        let half = (area.width - gap) * 0.5;
        self.file.layout(Rect::new(area.x, area.y + 3.0 * step, half, height));
        self.folder.layout(Rect::new(
            area.x + half + gap,
            area.y + 3.0 * step,
            half,
            height,
        ));
        self.error.layout(Rect::new(
            area.x,
            area.y + 4.0 * step,
            area.width,
            2.0 * height,
        ));
        let bottom = (area.y + area.height - height).max(area.y + 6.0 * step);
        let button_width = (2.0 * theme.spacing.xl).min(half);
        self.apply.layout(Rect::new(
            area.x + area.width - button_width,
            bottom,
            button_width,
            height,
        ));
        self.cancel.layout(Rect::new(
            area.x + area.width - 2.0 * button_width - gap,
            bottom,
            button_width,
            height,
        ));
    }
    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        // Open popup hit targets win over fields beneath the popup.
        if self.kind.is_open() && self.kind.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        if self.asset_mode()
            && self.choices.is_open()
            && self.choices.event(event, ctx) == EventResult::Handled
        {
            return EventResult::Handled;
        }
        if self.input.is_composing()
            && matches!(
                event,
                UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Escape, .. }
            )
        {
            self.input.event(event, ctx);
            return EventResult::Handled;
        }
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                (ctx.dispatch)(ResourceEditorAction::Cancel { id: self.draft.id }.action());
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Enter, .. } => {
                (ctx.dispatch)(ResourceEditorAction::Confirm { id: self.draft.id }.action());
                return EventResult::Handled;
            }
            _ => {}
        }

        for index in 0..self.child_count() {
            if self
                .child_mut(index)
                .is_some_and(|child| child.event(event, ctx) == EventResult::Handled)
            {
                return EventResult::Handled;
            }
        }
        match event {
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }
    fn paint(&self, ctx: &mut PaintContext) {
        self.surface.paint(self.bounds, self.card, ctx);
        for index in 0..self.child_count() {
            if let Some(child) = self.child(index) {
                child.paint(ctx);
            }
        }
    }
    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
    fn overlay_hit_test(&self, point: Point) -> bool {
        self.hit_test(point)
    }
    fn child_count(&self) -> usize {
        9
    }
    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.title),
            1 => Some(&self.kind),
            2 if self.draft.kind != ResourceKind::Unbound => Some(&self.input),
            3 if self.asset_mode() => Some(&self.choices),
            4 if self.draft.allows_browse(false) => Some(&self.file),
            5 if self.draft.allows_browse(true) => Some(&self.folder),
            6 => Some(&self.error),
            7 => Some(&self.cancel),
            8 => Some(&self.apply),
            _ => None,
        }
    }
    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.title),
            1 => Some(&mut self.kind),
            2 if self.draft.kind != ResourceKind::Unbound => Some(&mut self.input),
            3 if self.asset_mode() => Some(&mut self.choices),
            4 if self.draft.allows_browse(false) => Some(&mut self.file),
            5 if self.draft.allows_browse(true) => Some(&mut self.folder),
            6 => Some(&mut self.error),
            7 => Some(&mut self.cancel),
            8 => Some(&mut self.apply),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{AnimationTrackId, ParameterId, ParameterResourceContract};
    fn draft(reference: ParameterResourceReference) -> ResourceDraft {
        let schema = ParameterSchema::v1(
            ParameterId::new_static("test.resource"),
            PropertyValue::Resource(reference.clone()),
        )
        .with_cache_impact(mondrian_core::ParameterCacheImpact::Resource);
        ResourceDraft::new(
            ClipId::new(),
            InspectorPropertyTarget::Clip {
                parameter: AnimationParameterAddress {
                    animation_track_id: AnimationTrackId::new(),
                    parameter_id: schema.parameter_id.clone(),
                },
            },
            reference,
            schema,
            None,
            None,
        )
    }

    #[test]
    fn resource_dialog_router_establishes_text_focus_and_respects_ime_shortcuts() {
        use mondrian_ui_core::tree::WidgetTreeView;
        use mondrian_ui_core::FocusManager;
        use mondrian_ui_events::EventRouter;
        use std::cell::RefCell;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let draft = draft(ParameterResourceReference::Uri {
            uri: "https://example.test/original".to_owned(),
        });
        let draft_id = draft.id;
        let mut dialog =
            ResourceDialog::new(draft, "Resource".to_owned(), Vec::new(), AppUiLocale::EnUs);
        dialog.layout(Rect::new(0.0, 0.0, 800.0, 600.0));
        let input_id = dialog.input.id();
        let area = dialog.surface.content_rect(dialog.card);
        let point = {
            let theme = mondrian_ui_theme::current_theme();
            Point::new(
                area.x + 12.0,
                area.y
                    + 2.0 * (theme.spacing.interact_height + theme.spacing.md)
                    + theme.spacing.interact_height * 0.5,
            )
        };
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut router = EventRouter::new(dialog.id());
        {
            let mut tree = WidgetTreeView::new(&mut dialog);
            for event in [
                UiEvent::MouseDown {
                    position: point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                UiEvent::MouseUp {
                    position: point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
            ] {
                router.route(event, &mut tree, &dispatch);
            }
            assert_eq!(
                router.focus_manager().focused_widget(),
                Some(input_id),
                "modal click must establish real keyboard focus on the editable leaf"
            );
            router.route(
                UiEvent::MouseDown {
                    position: point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut tree,
                &dispatch,
            );
            assert_eq!(router.captured(), Some(input_id));
            router.route(
                UiEvent::MouseMove {
                    position: Point::new(area.x + area.width - 160.0, area.y + 270.0),
                    modifiers: Modifiers::none(),
                },
                &mut tree,
                &dispatch,
            );
            assert_eq!(
                router.captured(),
                Some(input_id),
                "normal siblings in the same overlay must not preempt text selection capture"
            );
            router.route(
                UiEvent::MouseUp {
                    position: point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut tree,
                &dispatch,
            );
            assert_eq!(router.captured(), None);
            assert_eq!(router.take_diagnostics().overlay_capture_preemptions, 0);
            router.route(UiEvent::ImePreedit("候选".to_owned()), &mut tree, &dispatch);
            for modifiers in [Modifiers::none(), Modifiers::shift()] {
                router.route(
                    UiEvent::KeyDown { key: KeyCode::Tab, modifiers },
                    &mut tree,
                    &dispatch,
                );
                assert_eq!(
                    router.focus_manager().focused_widget(),
                    Some(input_id),
                    "IME Tab must retain the composing input"
                );
            }
            router.route(
                UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut tree,
                &dispatch,
            );
            assert!(
                actions.borrow().is_empty(),
                "IME Enter must not apply a resource"
            );
            router.route(
                UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                &mut tree,
                &dispatch,
            );
            assert!(
                actions.borrow().is_empty(),
                "IME Escape must not cancel the dialog"
            );
            router.route(
                UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut tree,
                &dispatch,
            );
        }
        let actions = actions.borrow();
        assert_eq!(actions.len(), 1);
        let Action::Custom { payload, .. } = &actions[0] else {
            panic!("shell event");
        };
        let ResourceEditorAction::Confirm { id } =
            serde_json::from_value(payload.clone()).expect("confirm event")
        else {
            panic!("confirm");
        };
        assert_eq!(id, draft_id);
    }

    #[test]
    fn resource_draft_preserves_types_and_values_across_mode_changes() {
        for reference in [
            ParameterResourceReference::Unbound,
            ParameterResourceReference::ExternalFile {
                path: PathBuf::from("Unicode 文件 path with spaces.cube"),
            },
            ParameterResourceReference::Uri {
                uri: "https://example.test/文件?x=%20".to_owned()
            },
            ParameterResourceReference::ProjectAsset { asset_id: AssetId::new() },
        ] {
            let mut draft = draft(reference.clone());
            assert_eq!(draft.value().expect("original"), reference);
            assert!(draft.author_action(reference.clone()).is_none());
            let original_kind = draft.kind;
            draft.update(ResourceUpdate::Kind(ResourceKind::Unbound));
            assert_eq!(
                draft.value().expect("clear"),
                ParameterResourceReference::Unbound
            );
            draft.update(ResourceUpdate::Kind(original_kind));
            assert_eq!(draft.value().expect("restored draft"), reference);
        }
        assert_ne!(
            draft(ParameterResourceReference::Unbound).id,
            draft(ParameterResourceReference::Unbound).id
        );
    }

    #[test]
    fn unchanged_provider_reference_is_not_subject_to_new_input_validation() {
        let mut draft =
            draft(ParameterResourceReference::Uri { uri: "provider-owned-token".to_owned() });
        assert_eq!(draft.value(), Err("resource-uri-invalid"));
        assert_eq!(draft.confirmed_value(), Ok(None));
        draft.update(ResourceUpdate::Text("new invalid token".to_owned()));
        assert_eq!(draft.confirmed_value(), Err("resource-uri-invalid"));
        draft.update(ResourceUpdate::Kind(ResourceKind::Unbound));
        assert_eq!(
            draft.confirmed_value(),
            Ok(Some(ParameterResourceReference::Unbound))
        );
    }

    #[test]
    fn unavailable_original_asset_remains_visible_and_confirming_it_is_a_noop() {
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let reference = ParameterResourceReference::ProjectAsset { asset_id: AssetId::new() };
        let dialog = ResourceDialog::new(
            draft(reference),
            "Asset".to_owned(),
            Vec::new(),
            AppUiLocale::EnUs,
        );
        assert_eq!(dialog.choices.label(), "Unavailable asset");
        assert_eq!(dialog.draft.confirmed_value(), Ok(None));
    }

    #[test]
    fn resource_locale_refresh_preserves_draft_input_and_composition() {
        use mondrian_ui_core::tree::WidgetTreeView;
        use mondrian_ui_events::EventRouter;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let reference =
            ParameterResourceReference::Uri { uri: "https://example.test/原始".to_owned() };
        let mut dialog = ResourceDialog::new(
            draft(reference.clone()),
            "Resource".to_owned(),
            Vec::new(),
            AppUiLocale::EnUs,
        );
        dialog.layout(Rect::new(0.0, 0.0, 800.0, 600.0));
        let input_id = dialog.input.id();
        let id = dialog.draft.id;
        let theme = mondrian_ui_theme::current_theme();
        let area = dialog.surface.content_rect(dialog.card);
        let point = Point::new(
            area.x + 12.0,
            area.y
                + 2.0 * (theme.spacing.interact_height + theme.spacing.md)
                + theme.spacing.interact_height * 0.5,
        );
        drop(theme);
        let mut router = EventRouter::new(dialog.id());
        {
            let mut tree = WidgetTreeView::new(&mut dialog);
            router.route(
                UiEvent::MouseDown {
                    position: point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut tree,
                &|_| {},
            );
            router.route(
                UiEvent::MouseUp {
                    position: point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut tree,
                &|_| {},
            );
            router.route(UiEvent::ImePreedit("输入中".to_owned()), &mut tree, &|_| {});
        }
        dialog.set_error("resource-uri-invalid");
        dialog.set_locale(AppUiLocale::ZhCn, "资源".to_owned());
        assert_eq!(dialog.input.id(), input_id);
        assert_eq!(dialog.draft.id, id);
        assert!(dialog.is_composing());
        assert_eq!(
            dialog.draft.value().expect("unchanged reference"),
            reference
        );
        assert_eq!(
            dialog.error.text(),
            Localizer::new(AppUiLocale::ZhCn).expect("catalog").text("resource-uri-invalid")
        );
        assert_eq!(dialog.title.text(), "资源");
    }
    #[test]
    fn resource_uri_validation_keeps_the_authored_spelling() {
        let mut draft = draft(ParameterResourceReference::Unbound);
        draft.update(ResourceUpdate::Kind(ResourceKind::Uri));
        for uri in [
            "https://example.test/path?x=%20",
            "urn:example:résumé",
            "mailto:user@example.test",
            "plugin://vendor/resource",
        ] {
            draft.update(ResourceUpdate::Text(uri.to_owned()));
            assert_eq!(
                draft.value().expect("absolute URI"),
                ParameterResourceReference::Uri { uri: uri.to_owned() }
            );
        }
        for uri in [
            "",
            "relative/path",
            "https://example.test/a b",
            "https://example.test/\npath",
            "https://example.test/%",
            "https://example.test/%ZZ",
            "https:\\example.test/path",
        ] {
            draft.update(ResourceUpdate::Text(uri.to_owned()));
            assert_eq!(draft.value(), Err("resource-uri-invalid"));
            assert_eq!(draft.uri, uri, "validation must retain editable draft");
        }
    }
    #[test]
    fn resource_dialog_obeys_provider_kinds_and_preserves_input_identity() {
        let mut draft = draft(ParameterResourceReference::Unbound);
        draft.schema.resource = Some(ParameterResourceContract::external_file(vec![
            "cube".to_owned()
        ]));
        for locale in [AppUiLocale::EnUs, AppUiLocale::ZhCn, AppUiLocale::Pseudo] {
            let _guard = crate::app_ui::test_utils::theme_test_guard();
            let mut dialog =
                ResourceDialog::new(draft.clone(), "LUT".to_owned(), Vec::new(), locale);
            assert_eq!(dialog.kind.items().len(), 2);
            dialog.apply_update(ResourceUpdate::Kind(ResourceKind::Uri));
            assert_eq!(dialog.draft.kind, ResourceKind::Unbound);
            dialog.apply_update(ResourceUpdate::Kind(ResourceKind::ExternalFile));
            assert!(dialog.child(4).is_some());
            assert!(dialog.child(5).is_none());
            let input_id = dialog.input.id();
            dialog.apply_update(ResourceUpdate::Text("Unicode.cube".to_owned()));
            assert_eq!(
                dialog.input.id(),
                input_id,
                "text updates must not replace focused input"
            );
            assert!(dialog.draft.value().is_ok());
            dialog.apply_update(ResourceUpdate::Text("wrong.txt".to_owned()));
            assert_eq!(dialog.draft.value(), Err("resource-not-admitted"));
        }
    }

    #[test]
    fn resource_asset_search_has_localized_empty_results_without_losing_selection() {
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let selected = AssetId::new();
        let original = ParameterResourceReference::ProjectAsset { asset_id: selected };
        let mut dialog = ResourceDialog::new(
            draft(original.clone()),
            "Asset".to_owned(),
            vec![
                ResourceAssetChoice {
                    id: selected,
                    label: "Film · folder A/Shared.mov".to_owned(),
                },
                ResourceAssetChoice {
                    id: AssetId::new(),
                    label: "Film · folder B/Shared.mov".to_owned(),
                },
            ],
            AppUiLocale::EnUs,
        );
        assert_eq!(dialog.choices.items().len(), 2);
        let input = dialog.input.id();
        dialog.apply_update(ResourceUpdate::Search("FOLDER B".to_owned()));
        assert_eq!(dialog.choices.items().len(), 1);
        assert_eq!(dialog.input.id(), input);
        dialog.apply_update(ResourceUpdate::Search("missing".to_owned()));
        assert!(dialog.choices.items().is_empty());
        assert_eq!(dialog.error.text(), "No assets match this search.");
        assert_eq!(dialog.draft.value().expect("retained selection"), original);
        dialog.set_locale(AppUiLocale::ZhCn, "素材".to_owned());
        assert_eq!(dialog.error.text(), "没有匹配的素材。");
        dialog.apply_update(ResourceUpdate::Search(String::new()));
        assert_eq!(dialog.choices.items().len(), 2);
        assert!(dialog.error.text().is_empty());
    }
}
