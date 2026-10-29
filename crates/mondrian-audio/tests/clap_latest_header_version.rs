//! Independent immutable official CLAP 1.2.10 header-version admission.
//!
//! This existing public API control compiles before the ABI migration. A version
//! assertion failure is runtime RED; new missing extension types are not used to
//! manufacture a compile-only failure. Passing this control is not host feature
//! completeness, native lifecycle or performance qualification.

#[test]
fn clap_latest_header_version_matches_verified_official_1_2_10() {
    let actual = clack_host::utils::ClapVersion::CURRENT;
    // free-audio/clap immutable commit195b42a004144fab0b3cf95e9c067187d15365b7
    // include/clap/version.h defines these three actual official macros.
    assert_eq!((actual.major, actual.minor, actual.revision), (1, 2, 10));
}
