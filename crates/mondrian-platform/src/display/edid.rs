//! Minimal CTA-861 HDR Static Metadata parser used by the Linux DRM fallback.

/// HDR capability evidence encoded in an EDID CTA extension.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EdidHdrCapabilities {
    pub(crate) pq: bool,
    pub(crate) hlg: bool,
    pub(crate) max_luminance_nits: Option<u32>,
    pub(crate) min_luminance_millinits: Option<u32>,
}

/// Parse HDR Static Metadata Data Blocks from a complete EDID payload.
pub(crate) fn parse_hdr_capabilities(edid: &[u8]) -> Result<EdidHdrCapabilities, String> {
    if edid.len() < 128 || !edid.len().is_multiple_of(128) {
        return Err("EDID payload is not a whole number of 128-byte blocks".to_owned());
    }
    if edid[..8] != [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00] {
        return Err("EDID header is invalid".to_owned());
    }

    let declared_extensions = usize::from(edid[126]);
    let available_extensions = edid.len() / 128 - 1;
    let mut result = EdidHdrCapabilities::default();
    for extension_index in 0..declared_extensions.min(available_extensions) {
        let start = (extension_index + 1) * 128;
        let extension = &edid[start..start + 128];
        if extension[0] != 0x02 {
            continue;
        }

        let dtd_offset = usize::from(extension[2]);
        let data_end = if dtd_offset == 0 {
            127
        } else {
            dtd_offset.min(127)
        };
        let mut cursor = 4usize;
        while cursor < data_end {
            let header = extension[cursor];
            let tag = header >> 5;
            let length = usize::from(header & 0x1f);
            let block_end = cursor.saturating_add(1).saturating_add(length);
            if length == 0 || block_end > data_end {
                break;
            }
            let payload = &extension[cursor + 1..block_end];
            if tag == 0x07 && payload.first() == Some(&0x06) && payload.len() >= 2 {
                let eotf = payload[1];
                result.pq |= eotf & (1 << 2) != 0;
                result.hlg |= eotf & (1 << 3) != 0;

                if let Some(code) = payload.get(3).copied().filter(|code| *code != 0) {
                    let max_nits = 50.0 * 2f64.powf(f64::from(code) / 32.0);
                    result.max_luminance_nits = Some(max_nits.round() as u32);
                    if let Some(min_code) = payload.get(5).copied() {
                        let ratio = f64::from(min_code) / 255.0;
                        let min_nits = max_nits * ratio * ratio / 100.0;
                        result.min_luminance_millinits = Some((min_nits * 1000.0).round() as u32);
                    }
                }
            }
            cursor = block_end;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edid_with_hdr_block(eotf: u8, max_luminance: u8, min_luminance: u8) -> Vec<u8> {
        let mut edid = vec![0u8; 256];
        edid[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
        edid[126] = 1;
        let extension = &mut edid[128..];
        extension[0] = 0x02;
        extension[1] = 0x03;
        extension[2] = 11;
        extension[4..11].copy_from_slice(&[
            0xe6,
            0x06,
            eotf,
            0x01,
            max_luminance,
            0,
            min_luminance,
        ]);
        edid
    }

    #[test]
    fn parses_pq_hlg_and_luminance_from_cta_hdr_block() {
        let parsed = parse_hdr_capabilities(&edid_with_hdr_block(0b1100, 96, 32))
            .expect("synthetic EDID must parse");

        assert!(parsed.pq);
        assert!(parsed.hlg);
        assert_eq!(parsed.max_luminance_nits, Some(400));
        assert_eq!(parsed.min_luminance_millinits, Some(63));
    }

    #[test]
    fn valid_sdr_edid_reports_no_hdr_transfer_function() {
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);

        assert_eq!(
            parse_hdr_capabilities(&edid),
            Ok(EdidHdrCapabilities::default())
        );
    }

    #[test]
    fn malformed_edid_fails_without_inventing_capability() {
        assert!(parse_hdr_capabilities(&[0u8; 127]).is_err());
    }
}
