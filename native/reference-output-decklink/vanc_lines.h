// SPDX-License-Identifier: MIT
#pragma once
#include "bridge.h"
#include <vector>
namespace mondrian_decklink {
uint32_t row_bytes(uint32_t width);
void validate_packet(const MdDeckLinkWirePacket&, uint32_t width);
// Writes only an SDK-provided raw v210 line, after validating all placements.
void write_line(void* line, uint32_t width, const std::vector<MdDeckLinkWirePacket>& packets);
// Reads original words from the independently captured line. Never rebuilds
// checksum/parity from DID/payload, and never accepts truncated corrupt data.
std::vector<MdDeckLinkWirePacket> read_line(const void*, uint32_t width, uint32_t line_number);
}
