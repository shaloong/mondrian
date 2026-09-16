#include "vanc_raster.h"
#include <algorithm>
#include <cstring>
#include <limits>
#include <stdexcept>

namespace mondrian_aja {
namespace {
void require(bool condition, const char* detail) { if (!condition) throw std::runtime_error(detail); }
uint16_t parity(uint8_t value) {
    unsigned count = 0;
    for (unsigned n = value; n; n >>= 1) count += n & 1;
    return uint16_t(value | ((count & 1) << 8) | (((count & 1) ^ 1) << 9));
}
uint8_t data(uint16_t word) {
    require(word == parity(uint8_t(word)), "ST291 parity mismatch"); return uint8_t(word);
}
void geometry_check(const std::vector<uint32_t>& raster, const VancGeometry& geometry) {
    require(geometry.width >= 12 && geometry.width <= 4096 && geometry.height <= 4096
        && geometry.first_active > 0 && geometry.first_active < geometry.height
        && geometry.lines.size() == geometry.first_active
        && geometry.row_bytes % 4 == 0
        && uint64_t(geometry.row_bytes) >= ((uint64_t(geometry.width) * 2 + 2) / 3) * 4
        && uint64_t(raster.size()) * 4 == uint64_t(geometry.row_bytes) * geometry.height,
        "invalid VANC raster extent");
    auto lines = geometry.lines;
    std::sort(lines.begin(), lines.end());
    require(lines.front() != 0 && lines.back() <= 2047
        && std::adjacent_find(lines.begin(), lines.end()) == lines.end(), "ambiguous SMPTE VANC lines");
}
uint16_t component(const uint32_t* row, uint32_t index) {
    return uint16_t((row[index / 3] >> ((index % 3) * 10)) & 1023);
}
void component(uint32_t* row, uint32_t index, uint16_t word) {
    const unsigned shift = (index % 3) * 10;
    row[index / 3] = (row[index / 3] & ~(1023u << shift)) | (uint32_t(word) << shift);
}
}
void validate_component_packet(const std::vector<uint16_t>& words) {
    require(words.size() >= 7 && words.size() <= 262 && words[0] == 0
        && words[1] == 1023 && words[2] == 1023, "invalid ST291 ADF/extent");
    require(data(words[3]) < 128 && data(words[4]) != 0, "unsupported ST291 packet type");
    require(size_t(data(words[5])) + 7 == words.size(), "ST291 data count mismatch");
    uint16_t sum = 0;
    for (size_t n = 3; n + 1 < words.size(); ++n) {
        require(words[n] <= 1023 && (n < 6 || (words[n] > 3 && words[n] < 1020)),
            "protected or out-of-range ST291 data word");
        sum = uint16_t((sum + words[n]) & 511);
    }
    require(words.back() == uint16_t(sum | ((((sum >> 8) & 1) ^ 1) << 9)), "ST291 checksum mismatch");
}
void write_vanc_raster(std::vector<uint32_t>& raster, const VancGeometry& geometry,
    const std::vector<VancPacket>& packets) {
    geometry_check(raster, geometry);
    require(packets.size() <= 64, "VANC packet count exceeds bound");
    size_t total = 0;
    std::vector<std::vector<bool>> occupied(geometry.first_active, std::vector<bool>(geometry.width));
    // Validate the entire inventory before touching any raster word.
    for (const auto& packet : packets) {
        validate_component_packet(packet.words);
        total += packet.words.size(); require(total <= 16384, "VANC word budget exceeded");
        const auto at = std::find(geometry.lines.begin(), geometry.lines.end(), packet.line);
        require(at != geometry.lines.end() && uint64_t(packet.offset) + packet.words.size() <= geometry.width,
            "VANC packet lies outside represented luma interval");
        auto& row = occupied[size_t(at - geometry.lines.begin())];
        for (size_t n = packet.offset; n < packet.offset + packet.words.size(); ++n) {
            require(!row[n], "overlapping VANC packets"); row[n] = true;
        }
    }
    for (uint32_t row_index = 0; row_index < geometry.first_active; ++row_index) {
        auto* row = raster.data() + size_t(row_index) * (geometry.row_bytes / 4);
        for (uint32_t n = 0; n < geometry.width; ++n) {
            component(row, n * 2, 512); component(row, n * 2 + 1, 64);
        }
    }
    for (const auto& packet : packets) {
        const size_t row_index = size_t(std::find(geometry.lines.begin(), geometry.lines.end(), packet.line) - geometry.lines.begin());
        auto* row = raster.data() + row_index * (geometry.row_bytes / 4);
        for (size_t n = 0; n < packet.words.size(); ++n)
            component(row, (packet.offset + uint32_t(n)) * 2 + 1, packet.words[n]);
    }
}
std::vector<VancPacket> read_vanc_raster(const std::vector<uint32_t>& raster, const VancGeometry& geometry) {
    geometry_check(raster, geometry);
    std::vector<VancPacket> result;
    size_t total = 0;
    for (uint32_t row_index = 0; row_index < geometry.first_active; ++row_index) {
        const auto* row = raster.data() + size_t(row_index) * (geometry.row_bytes / 4);
        for (uint32_t offset = 0; offset + 2 < geometry.width; ++offset) {
            if (component(row, offset * 2 + 1) != 0 || component(row, (offset + 1) * 2 + 1) != 1023
                || component(row, (offset + 2) * 2 + 1) != 1023) continue;
            require(offset + 6 < geometry.width, "truncated on-wire ST291 header");
            const uint32_t extent = uint32_t(data(component(row, (offset + 5) * 2 + 1))) + 7;
            require(offset + extent <= geometry.width, "truncated on-wire ST291 packet");
            VancPacket packet{geometry.lines[row_index], offset, {}};
            for (uint32_t n = 0; n < extent; ++n) packet.words.push_back(component(row, (offset + n) * 2 + 1));
            validate_component_packet(packet.words);
            total += extent;
            require(result.size() < 64 && total <= 16384, "captured VANC inventory exceeds bound");
            result.push_back(std::move(packet)); offset += extent - 1;
        }
    }
    return result;
}
bool read_frame_marker(const VancPacket& packet, const uint8_t nonce[16], uint64_t& frame) {
    validate_component_packet(packet.words);
    if (data(packet.words[3]) != 0x5f || data(packet.words[4]) != 0x7f) return false;
    require(packet.words.size() == 39, "qualification marker has invalid extent");
    uint8_t payload[32]{};
    for (size_t n = 0; n < 32; ++n) payload[n] = data(packet.words[n + 6]);
    require(std::memcmp(payload, "MDANC001", 8) == 0 && std::memcmp(payload + 8, nonce, 16) == 0,
        "qualification marker belongs to another campaign");
    frame = 0;
    for (size_t n = 24; n < 32; ++n) frame = (frame << 8) | payload[n];
    return true;
}
} // namespace mondrian_aja
