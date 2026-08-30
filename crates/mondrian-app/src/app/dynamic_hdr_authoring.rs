//! Product Adapter for Sequence-owned Dynamic HDR authoring.

use super::AppState;
use mondrian_core::Result;
use mondrian_timeline::DynamicHdrAuthorEdit;

impl AppState {
    pub(super) fn dispatch_dynamic_hdr_product_action(
        &mut self,
        edit: DynamicHdrAuthorEdit,
    ) -> Result<()> {
        self.commit_active_sequence_edit("编辑 Dynamic HDR Program", move |sequence| {
            if !sequence.dynamic_hdr.apply(edit, sequence.settings.frame_rate)? {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "dynamic_hdr_authoring".to_owned(),
                    reason: "Dynamic HDR author edit is unchanged".to_owned(),
                });
            }
            Ok(sequence.id)
        })?;
        Ok(())
    }
}
