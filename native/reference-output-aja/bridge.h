#pragma once
#include <stdint.h>
#if defined(_WIN32) && defined(MD_AJA_BUILD)
#define MD_AJA_API extern "C" __declspec(dllexport)
#elif defined(_WIN32)
#define MD_AJA_API extern "C" __declspec(dllimport)
#else
#define MD_AJA_API extern "C"
#endif
// Versioned C ABI. No C++ object or exception crosses this boundary.
struct MdAjaDevice { uint64_t serial, generation; uint32_t mode_mask, flags; char driver[96], name[128]; };
struct MdAjaRequest { uint32_t mode, channels, ancillary, require_reference, preroll, max_frames; };
struct MdAjaPacket { uint32_t line, offset, space, did, sdid, length; uint8_t payload[256]; };
struct MdAjaEvent { uint32_t kind, reserved; uint64_t frame, ticks; };
struct MdAjaWirePacket { uint32_t line, offset, count, reserved; uint16_t words[262]; };
struct MdAjaWireFrame { uint64_t capture_ticks; uint32_t count, reserved; MdAjaWirePacket packets[64]; };
struct MdAjaShutdown { uint32_t requested, stopped, worker_joined, released; uint64_t outstanding_frames, outstanding_resources; char error[512]; };
static_assert(sizeof(MdAjaDevice) == 248);
static_assert(sizeof(MdAjaRequest) == 24);
static_assert(sizeof(MdAjaPacket) == 280);
static_assert(sizeof(MdAjaEvent) == 24);
static_assert(sizeof(MdAjaWirePacket) == 540);
static_assert(sizeof(MdAjaWireFrame) == 34576);
static_assert(sizeof(MdAjaShutdown) == 544);
MD_AJA_API uint32_t md_aja_abi_version();
MD_AJA_API int32_t md_aja_sdk_version(char*, uint32_t);
MD_AJA_API int32_t md_aja_discover(MdAjaDevice*, uint32_t, uint32_t*, char*, uint32_t);
MD_AJA_API int32_t md_aja_reference(uint64_t, uint64_t, int32_t*, char*, uint32_t);
// On failure a non-null session STILL belongs to the caller and must be shut down.
MD_AJA_API int32_t md_aja_open(uint64_t, uint64_t, const MdAjaRequest*, void**, char*, uint32_t);
MD_AJA_API int32_t md_aja_schedule(void*, uint64_t, const uint8_t*, uint32_t, uint32_t, const int32_t*, uint32_t, const MdAjaPacket*, uint32_t, char*, uint32_t);
MD_AJA_API int32_t md_aja_start(void*, char*, uint32_t);
MD_AJA_API int32_t md_aja_poll(void*, MdAjaEvent*, char*, uint32_t);
MD_AJA_API int32_t md_aja_request_stop(void*);
MD_AJA_API int32_t md_aja_shutdown(void*, MdAjaShutdown*);
// Exact luma VANC extension: independent input DMA returns captured words.
MD_AJA_API int32_t md_aja_schedule_vanc(void*, uint64_t, const uint8_t*, uint32_t, uint32_t,
    const int32_t*, uint32_t, const MdAjaWirePacket*, uint32_t, char*, uint32_t);
MD_AJA_API int32_t md_aja_capture_preflight(uint64_t, uint64_t, uint32_t, uint32_t, uint32_t, char*, uint32_t);
MD_AJA_API int32_t md_aja_capture_open(uint64_t, uint64_t, uint32_t, uint32_t, void**, char*, uint32_t);
MD_AJA_API int32_t md_aja_capture_poll(void*, MdAjaWireFrame*, char*, uint32_t);
MD_AJA_API int32_t md_aja_capture_start(void*, char*, uint32_t);
MD_AJA_API int32_t md_aja_capture_shutdown(void*, MdAjaShutdown*);
