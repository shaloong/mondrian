#include "vanc_raster.h"
#include <algorithm>
#include <iostream>
#include <stdexcept>

using namespace mondrian_aja;
static void check(bool ok, const char* detail) { if (!ok) throw std::runtime_error(detail); }
template<class F> static void rejects(F&& body) {
    bool rejected = false; try { body(); } catch (const std::exception&) { rejected = true; }
    check(rejected, "malformed VANC accepted");
}
int main() {
    try {
        VancGeometry geometry{1920, 1088, 8, 5120, {13,14,15,16,17,18,19,20}};
        const VancPacket packet{20, 0, {0,1023,1023,0x161,0x101,0x200,0x262}};
        // Exercise every v210 packing phase and the last representable position.
        for (uint32_t offset : {0u,1u,2u,3u,4u,5u,31u,127u,1913u}) {
            std::vector<uint32_t> raster(size_t(geometry.row_bytes / 4) * geometry.height, 0x01234567);
            const auto active = raster.begin() + size_t(geometry.first_active) * geometry.row_bytes / 4;
            auto placed = packet; placed.offset = offset;
            write_vanc_raster(raster, geometry, {placed});
            check(std::all_of(active, raster.end(), [](uint32_t word) { return word == 0x01234567; }), "VANC touched active picture");
            const auto decoded = read_vanc_raster(raster, geometry);
            check(decoded.size() == 1 && decoded[0].line == 20 && decoded[0].offset == offset
                && decoded[0].words == packet.words, "exact VANC placement did not roundtrip");
        }
        std::vector<uint32_t> raster(size_t(geometry.row_bytes / 4) * geometry.height, 0);
        const auto original = raster;
        rejects([&] { write_vanc_raster(raster, geometry, {packet, packet}); });
        check(raster == original, "rejected inventory partially mutated raster");
        auto invalid = packet; invalid.offset = UINT32_MAX;
        rejects([&] { write_vanc_raster(raster, geometry, {invalid}); });
        invalid = packet; invalid.line = 21;
        rejects([&] { write_vanc_raster(raster, geometry, {invalid}); });
        invalid = packet; invalid.words[3] ^= 0x100;
        rejects([&] { write_vanc_raster(raster, geometry, {invalid}); });
        invalid = packet; invalid.words.back() ^= 1;
        rejects([&] { write_vanc_raster(raster, geometry, {invalid}); });
        invalid = packet; invalid.words[5] = 0x101;
        rejects([&] { write_vanc_raster(raster, geometry, {invalid}); });
        auto bad_geometry = geometry; bad_geometry.lines[1] = 13;
        rejects([&] { write_vanc_raster(raster, bad_geometry, {}); });
        bad_geometry = geometry; bad_geometry.row_bytes = UINT32_MAX;
        rejects([&] { read_vanc_raster(raster, bad_geometry); });
        write_vanc_raster(raster, geometry, {packet});
        // Flip the actual raster's checksum bit, then scan the received raster.
        const size_t row = size_t(7) * geometry.row_bytes / 4;
        raster[row + 13 / 3] ^= 1u << ((13 % 3) * 10);
        rejects([&] { read_vanc_raster(raster, geometry); });
        std::cout << "{\"schema_version\":1,\"qualifying\":false,\"vanc_raster_checks\":\"passed\",\"physical_wire\":\"NotRun\"}\n";
        return 0;
    } catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
