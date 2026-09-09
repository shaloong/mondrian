// SPDX-License-Identifier: MIT
#pragma once
#include <stdint.h>
#if defined(MD_DECKLINK_BUILD)
#define MD_DL_API extern "C" __declspec(dllexport)
#else
#define MD_DL_API extern "C" __declspec(dllimport)
#endif
// API 12.0, bridge ABI 1. HRESULTs remain in diagnostic text; status values are
// stable: 0 success (poll: empty), 1 poll data/schedule full, -1 vendor failure, -2 driver missing,
// -3 device absent, -4 interface version mismatch, -5 invalid input, -6 busy.
struct MdDeckLinkDevice {
    uint64_t serial, generation, physical_group;
    uint32_t mode_mask, input_mode_mask, flags, maximum_audio_channels;
    char driver[96], name[128];
};
struct MdDeckLinkRequest { uint32_t mode, channels, ancillary, require_reference, preroll, max_frames; };
struct MdDeckLinkEvent { uint32_t kind, reserved; uint64_t frame, ticks; };
struct MdDeckLinkWirePacket { uint32_t line, offset, count, reserved; uint16_t words[262]; };
struct MdDeckLinkWireFrame { uint64_t capture_ticks; uint32_t count, reserved; MdDeckLinkWirePacket packets[64]; };
struct MdDeckLinkShutdown {
    uint32_t requested, stopped, worker_joined, released;
    uint64_t outstanding_frames, outstanding_resources; char error[512];
};
static_assert(sizeof(MdDeckLinkDevice) == 264);
static_assert(sizeof(MdDeckLinkRequest) == 24);
static_assert(sizeof(MdDeckLinkEvent) == 24);
static_assert(sizeof(MdDeckLinkWirePacket) == 540);
static_assert(sizeof(MdDeckLinkWireFrame) == 34576);
static_assert(sizeof(MdDeckLinkShutdown) == 544);
MD_DL_API uint32_t md_decklink_abi_version();
MD_DL_API int32_t md_decklink_sdk_version(char*, uint32_t);
MD_DL_API int32_t md_decklink_discover(MdDeckLinkDevice*, uint32_t, uint32_t*, char*, uint32_t);
MD_DL_API int32_t md_decklink_reference(uint64_t, uint64_t, int32_t*, char*, uint32_t);
// Every non-null returned owner MUST be consumed, including on open failure.
MD_DL_API int32_t md_decklink_open(uint64_t, uint64_t, const MdDeckLinkRequest*, void**, char*, uint32_t);
MD_DL_API int32_t md_decklink_schedule_vanc(void*, uint64_t, const uint8_t*, uint32_t, uint32_t,
    const int32_t*, uint32_t, const MdDeckLinkWirePacket*, uint32_t, char*, uint32_t);
MD_DL_API int32_t md_decklink_start(void*, char*, uint32_t);
MD_DL_API int32_t md_decklink_poll(void*, MdDeckLinkEvent*, char*, uint32_t);
MD_DL_API int32_t md_decklink_request_stop(void*);
MD_DL_API int32_t md_decklink_shutdown(void*, MdDeckLinkShutdown*);
MD_DL_API int32_t md_decklink_capture_preflight(uint64_t, uint64_t, uint32_t, uint32_t, uint32_t, char*, uint32_t);
MD_DL_API int32_t md_decklink_capture_open(uint64_t, uint64_t, uint32_t, uint32_t, void**, char*, uint32_t);
MD_DL_API int32_t md_decklink_capture_start(void*, char*, uint32_t);
MD_DL_API int32_t md_decklink_capture_poll(void*, MdDeckLinkWireFrame*, char*, uint32_t);
MD_DL_API int32_t md_decklink_capture_shutdown(void*, MdDeckLinkShutdown*);
