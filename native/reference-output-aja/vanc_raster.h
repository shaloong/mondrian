#pragma once
#include <cstddef>
#include <cstdint>
#include <vector>

namespace mondrian_aja {
// Full component words, including ADF and checksum. Raster codec owns no
// Timeline semantics and never regenerates an expected payload on capture.
struct VancPacket { uint32_t line, offset; std::vector<uint16_t> words; };
struct VancGeometry {
    uint32_t width, height, first_active, row_bytes;
    // One actual SMPTE line per pre-active raster row, obtained from SDK.
    std::vector<uint32_t> lines;
};
void validate_component_packet(const std::vector<uint16_t>& words);
void write_vanc_raster(std::vector<uint32_t>& raster, const VancGeometry& geometry,
    const std::vector<VancPacket>& packets);
std::vector<VancPacket> read_vanc_raster(const std::vector<uint32_t>& raster,
    const VancGeometry& geometry);
// Validation-only correlation data must be a real canonical ST291 packet.
// Its payload is MDANC001 + 128-bit campaign nonce + big-endian frame index.
bool read_frame_marker(const VancPacket& packet, const uint8_t nonce[16], uint64_t& frame);
} // namespace mondrian_aja
