//! Broadcast-delivery analysis and ancillary-data contracts.
//!
//! This crate is the platform-neutral Broadcast Compliance Module. It owns
//! versioned streaming QC evidence and canonical ancillary packets. Export,
//! reference-output, and vendor bridges consume these contracts without
//! independently interpreting QC thresholds or SMPTE packet syntax.

mod ancillary;
mod captions;
pub use captions::{
    import_caption_program, CaptionImportBinding, CaptionImportError, CaptionImportReceipt,
    CaptionSourceFormat, Cea608Operation,
};
mod artifact;
pub use artifact::{
    BroadcastArtifactQcError, BroadcastArtifactQcReport, BroadcastArtifactQcSession,
};
mod ancillary_program;
mod qc;
mod st436;
mod wire_qualification;
pub use ancillary_program::FrozenAncillaryProgram;
pub use st436::{
    decode_st436_ancillary, encode_st436_ancillary, read_st436_klv_frame, reimport_st436_canonical,
    verify_st436_mxf_ancillary, write_st436_klv_frame, St436Error, ST436_ANC_ESSENCE_KEY,
};
pub use wire_qualification::{
    AncillaryWireCorrelation, AncillaryWireError, CapturedAncillaryPacket,
};

pub use ancillary::{
    ActiveFormatDescription, AncillaryField, AncillaryFrame, AncillaryOrigin, AncillaryPacket,
    AncillaryPacketError, AncillaryPlacement, AncillarySpace, AncillaryValidationLevel,
    AtcPayloadType, AtcTimecode, BarData, CaptionDistributionPacket, St291Type2Packet,
};
pub use qc::{
    BroadcastQcError, BroadcastQcFinding, BroadcastQcFindingKind, BroadcastQcFrame,
    BroadcastQcObligation, BroadcastQcObligationKind, BroadcastQcObservationTap,
    BroadcastQcProfile, BroadcastQcReport, BroadcastQcRule, BroadcastQcRuleStatus,
    BroadcastQcSession, BroadcastQcSeverity, BroadcastQcVerdict, QcActivePicture,
};

mod regulatory_pse;
pub use regulatory_pse::{RegulatoryPseApproval, RegulatoryPseResponse, RegulatoryPseVerdict};
