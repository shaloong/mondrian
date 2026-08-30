//! Broadcast-delivery analysis and ancillary-data contracts.
//!
//! This crate is the platform-neutral Broadcast Compliance Module. It owns
//! versioned streaming QC evidence and canonical ancillary packets. Export,
//! reference-output, and vendor bridges consume these contracts without
//! independently interpreting QC thresholds or SMPTE packet syntax.

mod ancillary;
mod qc;

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
