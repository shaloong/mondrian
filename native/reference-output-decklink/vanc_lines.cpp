// SPDX-License-Identifier: MIT
#include "vanc_lines.h"
#include <cstring>
#include <stdexcept>
namespace mondrian_decklink {
namespace {
void require(bool condition, const char* text) { if (!condition) throw std::runtime_error(text); }
uint16_t parity(uint8_t value) {
    unsigned count = 0;
    for (unsigned n = value; n; n >>= 1) count += n & 1;
    return uint16_t(value | ((count & 1) << 8) | (((count & 1) ^ 1) << 9));
}
uint8_t data(uint16_t word) {
    require(word == parity(uint8_t(word)), "ST291 header parity mismatch"); return uint8_t(word);
}
uint16_t get(const uint8_t* row, uint32_t x) {
    const uint32_t component = 2 * x + 1;
    uint32_t word = 0; std::memcpy(&word, row + component / 3 * 4, 4);
    return uint16_t((word >> ((component % 3) * 10)) & 1023);
}
void put(uint8_t* row, uint32_t component, uint16_t value) {
    uint32_t word = 0; std::memcpy(&word, row + component / 3 * 4, 4);
    const uint32_t shift = (component % 3) * 10;
    word = (word & ~(1023u << shift)) | (uint32_t(value) << shift);
    std::memcpy(row + component / 3 * 4, &word, 4);
}
}
uint32_t row_bytes(uint32_t width) {
    require(width == 1920 || width == 1280, "unsupported DeckLink HD raster width");
    return ((width + 47) / 48) * 128;
}
void validate_packet(const MdDeckLinkWirePacket& packet, uint32_t width) {
    (void)row_bytes(width);
    require(packet.reserved == 0 && packet.line > 0 && packet.line <= 41 && packet.count >= 7
        && packet.count <= 262 && uint64_t(packet.offset) + packet.count <= width,
        "invalid progressive luma VANC extent");
    const auto* words = packet.words;
    require(words[0] == 0 && words[1] == 1023 && words[2] == 1023, "ST291 ADF mismatch");
    require(data(words[3]) < 128 && data(words[4]) != 0 && uint32_t(data(words[5])) + 7 == packet.count,
        "ST291 type/data count mismatch");
    uint16_t sum = 0;
    for (uint32_t n = 3; n + 1 < packet.count; ++n) {
        require(words[n] <= 1023 && (n < 6 || (words[n] > 3 && words[n] < 1020)), "ST291 protected data word");
        sum = uint16_t((sum + words[n]) & 511);
    }
    require(words[packet.count - 1] == uint16_t(sum | ((((sum >> 8) & 1) ^ 1) << 9)), "ST291 checksum mismatch");
}
void write_line(void* destination, uint32_t width, const std::vector<MdDeckLinkWirePacket>& packets) {
    const auto bytes = row_bytes(width);
    require(destination && packets.size() <= 64, "invalid VANC line owner");
    std::vector<bool> occupied(width);
    uint32_t line = 0;
    for (const auto& packet : packets) {
        validate_packet(packet, width);
        require(line == 0 || line == packet.line, "mixed lines in VANC line owner"); line = packet.line;
        for (uint32_t n = packet.offset; n < packet.offset + packet.count; ++n) {
            require(!occupied[n], "overlapping VANC packets"); occupied[n] = true;
        }
    }
    auto* row = static_cast<uint8_t*>(destination);
    std::memset(row, 0, bytes);
    for (uint32_t x = 0; x < width; ++x) { put(row, 2 * x, 512); put(row, 2 * x + 1, 64); }
    for (const auto& packet : packets)
        for (uint32_t n = 0; n < packet.count; ++n) put(row, 2 * (packet.offset + n) + 1, packet.words[n]);
}
std::vector<MdDeckLinkWirePacket> read_line(const void* source, uint32_t width, uint32_t line_number) {
    (void)row_bytes(width);
    require(source && line_number > 0 && line_number <= 41, "invalid captured VANC line");
    const auto* row = static_cast<const uint8_t*>(source);
    std::vector<MdDeckLinkWirePacket> result;
    for (uint32_t x = 0; x + 2 < width; ++x) {
        if (get(row, x) != 0 || get(row, x + 1) != 1023 || get(row, x + 2) != 1023) continue;
        require(x + 6 < width, "truncated captured ST291 header");
        const uint32_t count = uint32_t(data(get(row, x + 5))) + 7;
        require(x + count <= width && result.size() < 64, "captured ST291 extent exceeds bound");
        MdDeckLinkWirePacket packet{}; packet.line = line_number; packet.offset = x; packet.count = count;
        for (uint32_t n = 0; n < count; ++n) packet.words[n] = get(row, x + n);
        validate_packet(packet, width); result.push_back(packet); x += count - 1;
    }
    return result;
}
}
