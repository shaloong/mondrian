// This executable only enumerates devices and exercises rejected inputs.
// It never acquires a real card or starts video/audio playback.
#include "bridge.h"
#include <cstring>
#include <iostream>
#include <stdexcept>

static void check(bool condition, const char* detail) {
    if (!condition) throw std::runtime_error(detail);
}
int main() {
    try {
        char error[512]{};
        char version[64]{};
        check(md_aja_abi_version() == 1, "ABI version mismatch");
        check(md_aja_sdk_version(version, sizeof(version)) == 0 && std::strncmp(version, "18.1.0.", 7) == 0, "SDK version mismatch");
        check(md_aja_sdk_version(nullptr, 0) < 0, "null version accepted");
        MdAjaDevice devices[64]{};
        uint32_t count = 0;
        check(md_aja_discover(devices, 64, &count, error, sizeof(error)) == 0 && count <= 64, "native discovery failed");
        check(md_aja_discover(nullptr, 64, &count, error, sizeof(error)) < 0, "null inventory accepted");
        check(md_aja_discover(devices, UINT32_MAX, &count, error, sizeof(error)) < 0, "oversized inventory accepted");
        int32_t locked = -1;
        check(md_aja_reference(0, 0, &locked, error, sizeof(error)) < 0, "invalid serial reference accepted");
        check(md_aja_reference(0, 0, nullptr, error, sizeof(error)) < 0, "null reference accepted");
        check(md_aja_open(0, 0, nullptr, nullptr, error, sizeof(error)) < 0, "null open accepted");
        MdAjaEvent event{};
        check(md_aja_schedule(nullptr, 0, nullptr, 0, 0, nullptr, 0, nullptr, 0, error, sizeof(error)) < 0, "null schedule accepted");
        check(md_aja_start(nullptr, error, sizeof(error)) < 0, "null start accepted");
        check(md_aja_poll(nullptr, &event, error, sizeof(error)) < 0, "null poll accepted");
        check(md_aja_request_stop(nullptr) < 0, "null stop accepted");
        MdAjaShutdown shutdown{};
        check(md_aja_shutdown(nullptr, &shutdown) < 0, "null shutdown accepted");
        MdAjaWireFrame wire{};
        check(md_aja_capture_preflight(0, 0, 0, 20, 0, error, sizeof(error)) < 0, "invalid capture serial accepted");
        check(md_aja_capture_poll(nullptr, &wire, error, sizeof(error)) < 0, "null capture poll accepted");
        // Rejected opens may have allocated a card owner: consume every one.
        for (unsigned n = 0; n != 128; ++n) {
            MdAjaRequest request{0, 2, 0, 1, 2, 8};
            void* owner = nullptr;
            check(md_aja_open(0, 0, &request, &owner, error, sizeof(error)) < 0 && owner != nullptr, "invalid serial open ownership missing");
            check(md_aja_request_stop(owner) == 0, "rejected open stop failed");
            check(md_aja_shutdown(owner, &shutdown) == 0 && shutdown.requested == 1
                && shutdown.stopped == 1 && shutdown.worker_joined == 1 && shutdown.released == 1
                && shutdown.outstanding_frames == 0 && shutdown.outstanding_resources == 0,
                "rejected open leaked native resources");
            owner = nullptr;
            check(md_aja_capture_open(0, 0, 0, 8, &owner, error, sizeof(error)) < 0 && owner != nullptr,
                "invalid capture serial ownership missing");
            check(md_aja_capture_shutdown(owner, &shutdown) == 0 && shutdown.stopped == 1
                && shutdown.worker_joined == 1 && shutdown.released == 1 && shutdown.outstanding_resources == 0,
                "rejected capture open leaked resources");
        }
        std::cout << "{\"schema_version\":1,\"qualifying\":false,\"abi_checks\":\"passed\",\"rejected_open_cycles\":128,\"devices\":"
                  << count << ",\"sdk_version\":\"" << version << "\",\"physical_output\":\"NotRun\"}\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n'; return 1;
    }
}
