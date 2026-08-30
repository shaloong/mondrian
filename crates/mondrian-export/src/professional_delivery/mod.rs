//! Profile-qualified IMF, AS-11, and DCP delivery.
//!
//! This deep Module owns the standards-facing contract, package object graph,
//! external wrapping/inspection Adapters, validation, and package reimport.
//! Queue and App code consume this Interface and do not interpret XML, MXF
//! labels, digest algorithms, or profile names independently.

mod contract;
mod dcp_xyz;
mod package;
mod toolchain;

pub use contract::{
    resolve_professional_delivery, DeliverableLayout, ProfessionalDeliveryAdmissionError,
    ProfessionalEssenceKind, ResolvedProfessionalDeliveryContract,
};
pub use dcp_xyz::{encode_linear_rec709_as_dcdm_xyz12le, DcdmEncodingError};
pub use package::{
    build_and_validate_package, reimport_and_validate_package, CompositionPlaylistId,
    ImfTrackMetadata, PackageAssetEvidence, PackageAssetId, PackageAssetRole, PackageDigest,
    PackageDigestAlgorithm, PackageElementId, PackageInventory, PackageRelativePath, PackingListId,
    ProfessionalPackageBuildRequest, ProfessionalPackageDocumentIds, ProfessionalPackageError,
    ProfessionalPackageTrack, ValidatedProfessionalPackage,
};
pub use toolchain::{
    ProfessionalDeliveryTool, ProfessionalDeliveryToolIdentity, ProfessionalDeliveryToolchain,
    ProfessionalDeliveryToolchainError,
};

#[cfg(test)]
mod tests;
