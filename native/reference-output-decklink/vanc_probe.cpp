// SPDX-License-Identifier: MIT
#include "vanc_lines.h"
#include <cstring>
#include <iostream>
#include <stdexcept>
static void check(bool result, const char* message) { if (!result) throw std::runtime_error(message); }
static uint16_t parity(uint8_t value) {
    unsigned ones = 0; for (unsigned n = value; n; n >>= 1) ones += n & 1;
    return uint16_t(value | ((ones & 1) << 8) | (((ones & 1) ^ 1) << 9));
}
static MdDeckLinkWirePacket packet(uint32_t offset, uint8_t count) {
    MdDeckLinkWirePacket result{}; result.line = 20; result.offset = offset; result.count = uint32_t(count) + 7;
    result.words[0] = 0; result.words[1] = result.words[2] = 1023;
    result.words[3] = parity(0x61); result.words[4] = parity(1); result.words[5] = parity(count);
    for (uint32_t n = 0; n < count; ++n) result.words[n + 6] = parity(uint8_t(n));
    uint16_t sum = 0; for (uint32_t n = 3; n + 1 < result.count; ++n) sum = uint16_t((sum + result.words[n]) & 511);
    result.words[result.count - 1] = uint16_t(sum | ((((sum >> 8) & 1) ^ 1) << 9)); return result;
}
template<class F> static void rejected(F fn) { bool failed = false; try { fn(); } catch (const std::exception&) { failed = true; } check(failed, "invalid VANC accepted"); }
int main() {
    try {
        for (const uint32_t width : {1280u, 1920u}) {
            std::vector<uint8_t> row(mondrian_decklink::row_bytes(width) + 32, 0xa5);
            const auto first = packet(0, 255); const auto last = packet(width - 8, 1);
            mondrian_decklink::write_line(row.data(), width, {first, last});
            const auto read = mondrian_decklink::read_line(row.data(), width, 20);
            check(read.size() == 2 && std::memcmp(&read[0], &first, sizeof(first)) == 0
                && std::memcmp(&read[1], &last, sizeof(last)) == 0, "captured raw words/offset changed");
            for (size_t n = mondrian_decklink::row_bytes(width); n < row.size(); ++n) check(row[n] == 0xa5, "VANC line write overflow");
            const auto before = row;
            rejected([&] { mondrian_decklink::write_line(row.data(), width, {first, first}); });
            check(before == row, "rejected placement partially wrote line");
            auto corrupt = first; corrupt.words[3] ^= 256;
            rejected([&] { mondrian_decklink::validate_packet(corrupt, width); });
            corrupt = first; corrupt.words[corrupt.count - 1] ^= 1;
            rejected([&] { mondrian_decklink::validate_packet(corrupt, width); });
            corrupt = first; corrupt.offset = UINT32_MAX;
            rejected([&] { mondrian_decklink::validate_packet(corrupt, width); });
            corrupt = first; corrupt.count = 263;
            rejected([&] { mondrian_decklink::validate_packet(corrupt, width); });
            // Corrupt a captured checksum component in the original line bytes.
            const uint32_t component = 2 * (last.offset + last.count - 1) + 1;
            row[component / 3 * 4 + (component % 3 * 10) / 8] ^= uint8_t(1u << ((component % 3 * 10) % 8));
            rejected([&] { (void)mondrian_decklink::read_line(row.data(), width, 20); });
        }
        rejected([] { (void)mondrian_decklink::row_bytes(UINT32_MAX); });
        std::cout << "{\"qualifying\":false,\"vanc_boundary_checks\":\"passed\",\"physical_readback\":\"NotRun\"}\n"; return 0;
    } catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
