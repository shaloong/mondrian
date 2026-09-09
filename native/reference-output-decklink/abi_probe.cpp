// SPDX-License-Identifier: MIT
// Read-only discovery and rejected opens. No real device acquisition/playback.
#include "bridge.h"
#include <cstring>
#include <iostream>
#include <stdexcept>
static void check(bool result, const char* message) { if (!result) throw std::runtime_error(message); }
int main() {
    try {
        char error[512]{}, version[64]{};
        check(md_decklink_abi_version() == 1, "ABI version");
        check(md_decklink_sdk_version(version, sizeof(version)) == 0 && std::strcmp(version, "12.0") == 0, "pinned API version");
        check(md_decklink_sdk_version(nullptr, 0) == -5, "null SDK buffer");
        MdDeckLinkDevice devices[64]{}; uint32_t count = 0;
        const int32_t discovered = md_decklink_discover(devices, 64, &count, error, sizeof(error));
        check(discovered == 0 || discovered == -2 || discovered == -4, "unexpected discovery failure");
        check(count <= 64, "inventory overflow");
        check(md_decklink_discover(nullptr, 64, &count, error, sizeof(error)) == -5, "null inventory");
        check(md_decklink_discover(devices, UINT32_MAX, &count, error, sizeof(error)) == -5, "unbounded inventory");
        check(md_decklink_open(0, 0, nullptr, nullptr, error, sizeof(error)) == -5, "null open");
        check(md_decklink_start(nullptr, error, sizeof(error)) == -5, "null start");
        check(md_decklink_schedule_vanc(nullptr, 0, nullptr, 0, 0, nullptr, 0, nullptr, 0, error, sizeof(error)) == -5, "null schedule");
        check(md_decklink_capture_preflight(0, 0, UINT32_MAX, 20, 0, error, sizeof(error)) == -5, "invalid capture mode");
        check(md_decklink_capture_preflight(0, 0, 8, 26, 0, error, sizeof(error)) == -5, "active-picture marker");
        check(md_decklink_capture_preflight(0, 0, 0, 20, UINT32_MAX, error, sizeof(error)) == -5, "overflowing marker");
        MdDeckLinkShutdown receipt{};
        check(md_decklink_shutdown(nullptr, &receipt) == -5, "null shutdown");
        for (uint32_t n = 0; n < 128; ++n) {
            MdDeckLinkRequest request{0, 2, 0, 1, 2, 8}; void* owner = nullptr;
            check(md_decklink_open(0, 0, &request, &owner, error, sizeof(error)) == -5 && owner, "failed output owner missing");
            MdDeckLinkEvent event{};
            check(md_decklink_poll(owner, &event, error, sizeof(error)) < 0, "failed output owner accepted poll");
            check(md_decklink_request_stop(owner) == 0, "rejected output stop");
            check(md_decklink_shutdown(owner, &receipt) == 0 && receipt.requested && receipt.stopped
                && receipt.worker_joined && receipt.released && !receipt.outstanding_frames && !receipt.outstanding_resources, "failed output owner leaked");
            owner = nullptr;
            check(md_decklink_capture_open(0, 0, 0, 8, &owner, error, sizeof(error)) == -5 && owner, "failed input owner missing");
            MdDeckLinkWireFrame frame{};
            check(md_decklink_capture_poll(owner, &frame, error, sizeof(error)) < 0, "failed capture owner accepted poll");
            check(md_decklink_capture_shutdown(owner, &receipt) == 0 && receipt.requested && receipt.stopped
                && receipt.worker_joined && receipt.released && !receipt.outstanding_resources, "failed input owner leaked");
        }
        std::cout << "{\"schema_version\":1,\"qualifying\":false,\"api_version\":\"12.0\",\"abi_checks\":\"passed\",\"failed_open_cycles\":128,\"discovery_status\":"
            << discovered << ",\"devices\":" << count << ",\"physical_output\":\"NotRun\",\"reason\":\""
            << (discovered == -2 ? "DesktopVideoDriverMissing" : discovered == -4 ? "DriverInterfaceMismatch" : count ? "NoPhysicalCampaignRequested" : "NoDeckLinkDevices") << "\"}\n";
        return 0;
    } catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
