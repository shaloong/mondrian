//! Inspector Adapter for exact audio Component channel matrices.
//!
//! Timeline owns persistent mapping intent and atomic validation. This Module
//! combines that intent with current media/nested dependency evidence and
//! translates deliberate user choices into the closed audio Product Action.

use mondrian_core::{
    AudioChannelLayout, AudioChannelMixMatrix, AudioComponentEditId, MAX_AUDIO_CHANNEL_MIX_GAIN,
};
use mondrian_editor_state::Action;
use mondrian_timeline::audio::AudioComponentChannelMapping;
use mondrian_timeline::{AudioComponentAddress, AudioComponentEditRequest, AudioComponentMutation};
use mondrian_ui_widgets::{Dropdown, Label, MenuItem, NumberInput, PropertyRow, PropertySection};

use crate::app::ui_actions::audio_component_edit_action;
use crate::app::SelectedClipRef;

/// Exact author policy, dependency evidence, and reviewable matrix projection.
#[derive(Debug, Clone)]
pub(crate) struct AudioChannelMappingModel {
    pub(crate) mapping: AudioComponentChannelMapping,
    pub(crate) observed_source_layout: Option<AudioChannelLayout>,
    pub(crate) destination_layout: AudioChannelLayout,
    pub(crate) review_matrix: Option<AudioChannelMixMatrix>,
    pub(crate) matrix_is_explicit: bool,
    pub(crate) diagnostic: Option<String>,
}

/// Combine persistent author intent with current exact dependency evidence.
pub(crate) fn project_audio_channel_mapping(
    mapping: &AudioComponentChannelMapping,
    observed_source_layout: Option<AudioChannelLayout>,
    destination_layout: AudioChannelLayout,
) -> AudioChannelMappingModel {
    let (review_matrix, matrix_is_explicit, diagnostic) = match mapping {
        AudioComponentChannelMapping::Standard => match observed_source_layout {
            Some(source_layout) => {
                match AudioChannelMixMatrix::standard(source_layout, destination_layout) {
                    Ok(matrix) => (Some(matrix), false, None),
                    Err(error) => (
                        None,
                        false,
                        Some(format!("标准映射不可用：{error}；播放和导出会拒绝猜测")),
                    ),
                }
            }
            None => (
                None,
                false,
                Some("当前依赖没有可证明的精确源布局；播放和导出会拒绝猜测".to_owned()),
            ),
        },
        AudioComponentChannelMapping::Explicit(matrix) => {
            let diagnostic = match observed_source_layout {
                Some(observed) if observed != matrix.source_layout() => Some(format!(
                    "显式矩阵要求 {}，当前依赖为 {}；播放和导出会拒绝不匹配的信号",
                    matrix.source_layout(),
                    observed
                )),
                None => Some(
                    "显式矩阵已保留，但当前依赖没有可证明的源布局；执行会保持 fail-closed"
                        .to_owned(),
                ),
                Some(_) => None,
            };
            (Some(matrix.clone()), true, diagnostic)
        }
    };
    AudioChannelMappingModel {
        mapping: mapping.clone(),
        observed_source_layout,
        destination_layout,
        review_matrix,
        matrix_is_explicit,
        diagnostic,
    }
}

/// Append mapping policy, layout evidence, and sparse coefficient controls.
pub(crate) fn with_audio_channel_mapping_rows(
    mut section: PropertySection,
    selection: Option<SelectedClipRef>,
    edit_id: AudioComponentEditId,
    model: &AudioChannelMappingModel,
    can_edit: bool,
) -> PropertySection {
    let mode_label = match model.mapping {
        AudioComponentChannelMapping::Standard => "标准（自动）",
        AudioComponentChannelMapping::Explicit(_) => "自定义（显式矩阵）",
    };
    let mut mode_items = vec![MenuItem::new(
        "标准（自动，执行时 fail-closed）",
        audio_component_mutation_action(
            selection,
            edit_id,
            AudioComponentMutation::SetChannelMapping {
                value: AudioComponentChannelMapping::Standard,
            },
        ),
    )
    .checked(matches!(
        model.mapping,
        AudioComponentChannelMapping::Standard
    ))];
    if let Some(source_layout) = model.observed_source_layout {
        if let Ok(matrix) = AudioChannelMixMatrix::standard(source_layout, model.destination_layout)
        {
            let checked = matches!(
                &model.mapping,
                AudioComponentChannelMapping::Explicit(current) if current == &matrix
            );
            mode_items.push(
                MenuItem::new(
                    "自定义：从当前标准矩阵建立快照",
                    audio_component_mutation_action(
                        selection,
                        edit_id,
                        AudioComponentMutation::SetChannelMapping {
                            value: AudioComponentChannelMapping::Explicit(matrix),
                        },
                    ),
                )
                .checked(checked),
            );
        }
        if let Ok(matrix) = AudioChannelMixMatrix::new(source_layout, model.destination_layout, [])
        {
            mode_items.push(MenuItem::new(
                "自定义：空白矩阵",
                audio_component_mutation_action(
                    selection,
                    edit_id,
                    AudioComponentMutation::SetChannelMapping {
                        value: AudioComponentChannelMapping::Explicit(matrix),
                    },
                ),
            ));
        }
    }
    section = section.with_row(PropertyRow::new(
        "通道映射",
        Box::new(
            Dropdown::new(mode_label, mode_items)
                .with_max_visible_items(8)
                .enabled(can_edit),
        ),
    ));

    let authored_source = match &model.mapping {
        AudioComponentChannelMapping::Explicit(matrix) => Some(matrix.source_layout()),
        AudioComponentChannelMapping::Standard => model.observed_source_layout,
    };
    section = section.with_row(PropertyRow::new(
        "信号布局",
        Box::new(
            Label::new(authored_source.map_or_else(
                || format!("未知 → {}", model.destination_layout),
                |source| format!("{} → {}", source, model.destination_layout),
            ))
            .muted(),
        ),
    ));
    if let Some(diagnostic) = &model.diagnostic {
        section = section.with_row(PropertyRow::new(
            "映射诊断",
            Box::new(Label::new(diagnostic.clone()).muted()),
        ));
    }

    let Some(matrix) = &model.review_matrix else {
        return section;
    };
    for entry in matrix.entries() {
        let source = entry.source_channel();
        let destination = entry.destination_channel();
        let row_label = format!(
            "{} → {}",
            channel_label(matrix.source_layout(), source),
            channel_label(matrix.destination_layout(), destination)
        );
        if model.matrix_is_explicit {
            let source_layout = matrix.source_layout();
            let destination_layout = matrix.destination_layout();
            section = section.with_row(PropertyRow::new(
                row_label,
                Box::new(
                    NumberInput::new(
                        entry.gain().get(),
                        -MAX_AUDIO_CHANNEL_MIX_GAIN,
                        MAX_AUDIO_CHANNEL_MIX_GAIN,
                    )
                    .with_step(0.01)
                    .with_decimals(3)
                    .enabled(can_edit)
                    .on_change(move |gain| {
                        matrix_gain_action(
                            selection,
                            edit_id,
                            source_layout,
                            destination_layout,
                            source,
                            destination,
                            gain,
                        )
                    }),
                ),
            ));
        } else {
            section = section.with_row(PropertyRow::new(
                row_label,
                Box::new(Label::new(format!("{:.6}", entry.gain().get())).muted()),
            ));
        }
    }

    if model.matrix_is_explicit {
        for destination in 0..matrix.destination_layout().channel_count_u8() {
            let options = (0..matrix.source_layout().channel_count_u8())
                .filter(|source| {
                    !matrix.entries().iter().any(|entry| {
                        entry.source_channel() == *source
                            && entry.destination_channel() == destination
                    })
                })
                .map(|source| {
                    MenuItem::new(
                        channel_label(matrix.source_layout(), source),
                        matrix_gain_action(
                            selection,
                            edit_id,
                            matrix.source_layout(),
                            matrix.destination_layout(),
                            source,
                            destination,
                            1.0,
                        ),
                    )
                })
                .collect::<Vec<_>>();
            if !options.is_empty() {
                section = section.with_row(PropertyRow::new(
                    format!(
                        "添加至 {}",
                        channel_label(matrix.destination_layout(), destination)
                    ),
                    Box::new(
                        Dropdown::new("选择源通道…", options)
                            .with_max_visible_items(12)
                            .enabled(can_edit),
                    ),
                ));
            }
        }
    }
    section
}

/// Build the sole Product Action for any placement-local Component mutation.
pub(crate) fn audio_component_mutation_action(
    selection: Option<SelectedClipRef>,
    edit_id: AudioComponentEditId,
    mutation: AudioComponentMutation,
) -> Option<Action> {
    selection.filter(|selection| !selection.is_video_track).map(|selection| {
        audio_component_edit_action(AudioComponentEditRequest {
            address: AudioComponentAddress {
                track_id: selection.track_id,
                clip_id: selection.clip_id,
                edit_id,
            },
            mutation,
        })
    })
}

fn matrix_gain_action(
    selection: Option<SelectedClipRef>,
    edit_id: AudioComponentEditId,
    source_layout: AudioChannelLayout,
    destination_layout: AudioChannelLayout,
    source_channel: u8,
    destination_channel: u8,
    gain: f64,
) -> Option<Action> {
    if !gain.is_finite() || gain.abs() > MAX_AUDIO_CHANNEL_MIX_GAIN {
        return None;
    }
    audio_component_mutation_action(
        selection,
        edit_id,
        AudioComponentMutation::SetExplicitChannelMixGain {
            expected_source_layout: source_layout,
            expected_destination_layout: destination_layout,
            source_channel,
            destination_channel,
            gain,
        },
    )
}

fn channel_label(layout: AudioChannelLayout, channel: u8) -> String {
    layout.channel_position(usize::from(channel)).map_or_else(
        || format!("Ch {}", channel + 1),
        |position| position.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{ClipId, TrackId};

    #[test]
    fn projection_preserves_fail_closed_layout_evidence() {
        let unsupported = project_audio_channel_mapping(
            &AudioComponentChannelMapping::Standard,
            Some(AudioChannelLayout::Surround51Back),
            AudioChannelLayout::Stereo,
        );
        assert!(unsupported.review_matrix.is_none());
        assert!(unsupported
            .diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("拒绝猜测")));

        let explicit = AudioChannelMixMatrix::identity(AudioChannelLayout::Mono);
        let mismatch = project_audio_channel_mapping(
            &AudioComponentChannelMapping::Explicit(explicit.clone()),
            Some(AudioChannelLayout::Stereo),
            AudioChannelLayout::Mono,
        );
        assert_eq!(mismatch.review_matrix, Some(explicit));
        assert!(mismatch.matrix_is_explicit);
        assert!(mismatch
            .diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("拒绝不匹配")));
    }

    #[test]
    fn coefficient_action_rebuilds_one_canonical_complete_matrix() {
        let selection = SelectedClipRef {
            track_id: TrackId::new(),
            is_video_track: false,
            clip_id: ClipId::new(),
        };
        let edit_id = AudioComponentEditId::new();
        let matrix = AudioChannelMixMatrix::identity(AudioChannelLayout::Stereo);
        let action = matrix_gain_action(
            Some(selection),
            edit_id,
            matrix.source_layout(),
            matrix.destination_layout(),
            0,
            0,
            0.0,
        )
        .expect("remove one edge");
        let Action::Custom { payload, .. } = action else {
            panic!("expected external product Action");
        };
        let request: AudioComponentEditRequest =
            serde_json::from_value(payload).expect("typed request");
        let AudioComponentMutation::SetExplicitChannelMixGain {
            expected_source_layout,
            expected_destination_layout,
            source_channel,
            destination_channel,
            gain,
        } = request.mutation
        else {
            panic!("expected one guarded coefficient mutation");
        };
        assert_eq!(expected_source_layout, AudioChannelLayout::Stereo);
        assert_eq!(expected_destination_layout, AudioChannelLayout::Stereo);
        assert_eq!(source_channel, 0);
        assert_eq!(destination_channel, 0);
        assert_eq!(gain, 0.0);
        assert!(matrix_gain_action(
            Some(selection),
            edit_id,
            matrix.source_layout(),
            matrix.destination_layout(),
            0,
            0,
            17.0,
        )
        .is_none());
    }
}
