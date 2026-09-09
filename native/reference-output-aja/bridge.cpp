// SPDX-License-Identifier: MIT
// Native AJA NTV2 owner. Built against the pinned official libajantv2 source.
#include "bridge.h"
#include "vanc_raster.h"
#include "ntv2card.h"
#include "ntv2devicescanner.h"
#include "ntv2formatdescriptor.h"
#include "ntv2signalrouter.h"
#include "ntv2utils.h"
#include "ntv2version.h"
#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstring>
#include <deque>
#include <limits>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>
#include <Windows.h>

namespace {
constexpr NTV2Channel channel = NTV2_CHANNEL1;
constexpr NTV2AudioSystem audio_system = NTV2_AUDIOSYSTEM_1;
constexpr ULWord owner_signature = 0x4D444E41; // MDNA
constexpr NTV2VideoFormat formats[] = {
    NTV2_FORMAT_1080p_2398, NTV2_FORMAT_1080p_2400, NTV2_FORMAT_1080p_2500,
    NTV2_FORMAT_1080p_2997, NTV2_FORMAT_1080p_3000, NTV2_FORMAT_1080p_5000_A,
    NTV2_FORMAT_1080p_5994_A, NTV2_FORMAT_1080p_6000_A,
    NTV2_FORMAT_720p_5000, NTV2_FORMAT_720p_5994, NTV2_FORMAT_720p_6000,
};
constexpr size_t format_count = sizeof(formats) / sizeof(formats[0]);
void require(bool ok, const char* operation) { if (!ok) throw std::runtime_error(operation); }
void text(char* buffer, uint32_t size, const std::string& value) {
    if (!buffer || !size) return;
    const size_t count = std::min<size_t>(value.size(), size - 1);
    std::memcpy(buffer, value.data(), count); buffer[count] = 0;
}
template<class F> int32_t boundary(F&& body, char* error, uint32_t size) noexcept {
    try { body(); return 0; }
    catch (const std::exception& ex) { text(error, size, ex.what()); }
    catch (...) { text(error, size, "unknown native AJA exception"); }
    return -1;
}
uint64_t fingerprint(const std::string& value) {
    uint64_t hash = 14695981039346656037ULL;
    for (const unsigned char byte : value) { hash ^= byte; hash *= 1099511628211ULL; }
    return hash ? hash : 1;
}
uint32_t mode_mask(CNTV2Card& card) {
    if (!card.features().CanDoPlayback() || !card.features().CanDoFrameStore1Display()
        || !card.features().CanDoFrameBufferFormat(NTV2_FBF_10BIT_YCBCR)
        || !card.features().GetNumVideoOutputs() || card.features().GetMaxAudioChannels() < 8 || !card.features().GetNumAudioSystems() || card.features().CanDo2110()) return 0;
    uint32_t mask = 0;
    for (size_t n = 0; n < format_count; ++n)
        if (card.features().CanDoVideoFormat(formats[n])) mask |= 1u << n;
    return mask;
}
MdAjaDevice describe(CNTV2Card& card) {
    MdAjaDevice device{};
    device.serial = card.GetSerialNumber();
    require(device.serial != 0, "AJA device has no stable serial identity");
    device.mode_mask = mode_mask(card);
    const auto genlock = card.features().GetGenlockVersion();
    device.flags = (card.features().CanDoCustomAnc() ? 1u : 0u) | ((genlock == 2 || genlock == 3) ? 2u : 0u);
    const std::string driver = card.GetDriverVersionString();
    require(!driver.empty(), "AJA driver version unavailable");
    text(device.driver, sizeof(device.driver), driver);
    text(device.name, sizeof(device.name), card.GetDisplayName());
    device.generation = fingerprint(std::to_string(device.serial) + ":" + std::to_string(card.GetDeviceID())
        + ":" + driver + ":" + std::to_string(device.mode_mask) + ":" + std::to_string(device.flags));
    return device;
}
void open_exact(CNTV2Card& card, uint64_t serial, uint64_t generation) {
    const auto devices = CNTV2DeviceScanner::GetDeviceInfoList();
    for (const auto& info : devices) {
        CNTV2Card candidate;
        require(info.deviceIndex <= UINT16_MAX, "AJA device index exceeds SDK bounds");
        if (!candidate.Open(UWord(info.deviceIndex))) continue;
        if (candidate.GetSerialNumber() != serial) continue;
        const auto device = describe(candidate);
        require(device.generation == generation, "AJA device generation changed");
        candidate.Close();
        require(card.Open(UWord(info.deviceIndex)), "AJA exact device reopen failed");
        require(card.GetSerialNumber() == serial && describe(card).generation == generation,
            "AJA device identity changed during open");
        return;
    }
    throw std::runtime_error("AJA exact serial device is unavailable");
}
int32_t reference_lock(CNTV2Card& card) {
    const auto genlock = card.features().GetGenlockVersion();
    if (genlock != 2 && genlock != 3) return -1;
    NTV2ReferenceSource source = NTV2_REFERENCE_FREERUN;
    ULWord status = 0;
    require(card.GetReference(source) && card.ReadRegister(48, status), "AJA hardware reference readback failed");
    // Official pinned driver/ntv2genlock.c and ntv2genlock2.c: register 48,
    // bits 24..27 hardware source, bit 30 reference present, bit 31 PLL locked.
    return source == NTV2_REFERENCE_EXTERNAL && ((status >> 24) & 15) == 0
        && (status & 0xC0000000u) == 0xC0000000u ? 1 : 0;
}
struct Frame {
    uint64_t index = 0;
    std::vector<uint32_t> video, audio;
};
mondrian_aja::VancGeometry vanc_geometry(const NTV2FormatDescriptor& format) {
    mondrian_aja::VancGeometry geometry{format.GetRasterWidth(), format.GetRasterHeight(),
        format.GetFirstActiveLine(), format.GetBytesPerRow(), {}};
    require(geometry.first_active > 0, "AJA format has no captured VANC raster");
    for (ULWord row = 0; row < geometry.first_active; ++row) {
        ULWord line = 0; bool field2 = false;
        require(format.GetSMPTELineNumber(row, line, field2) && !field2, "AJA VANC line has no progressive SMPTE mapping");
        geometry.lines.push_back(line);
    }
    return geometry;
}
struct SavedConfiguration {
    NTV2TaskMode task{}; NTV2Mode mode{}; NTV2VideoFormat video{};
    NTV2FrameBufferFormat pixels{}; NTV2VANCMode vanc{}; NTV2VANCDataShiftMode vanc_shift{};
    NTV2AudioRate rate{}; NTV2AudioBufferSize buffer{}; NTV2AudioLoopBack loopback{}; NTV2EmbeddedAudioClock audio_clock{};
    NTV2AudioSystem embed{}; ULWord channels = 0;
    NTV2VPIDTransferCharacteristics transfer{}; NTV2VPIDColorimetry color{}; NTV2VPIDLuminance luminance{};
    NTV2Standard standard{}; NTV2XptConnections routing;
    bool enabled = false, transmit = false, three_g = false, three_g_b = false, bidirectional = false;
    bool captured = false;
    void capture(CNTV2Card& card) {
        bool multi = false;
        if (card.features().CanDoMultiFormat()) require(card.GetMultiFormatMode(multi) && !multi, "AJA multiformat configuration is not admitted");
        for (UWord n = 0; n < card.features().GetNumFrameStores(); ++n) {
            AUTOCIRCULATE_STATUS status;
            require(card.AutoCirculateGetStatus(NTV2Channel(n), status) && status.IsStopped(), "another AJA AutoCirculate owner is active");
            bool other_enabled = false;
            if (n != 0) require(card.IsChannelEnabled(NTV2Channel(n), other_enabled) && !other_enabled,
                "another AJA FrameStore is enabled; whole-device output is not admitted");
        }
        bool override_enabled = false;
        NTV2VPIDTransferCharacteristics override_transfer{};
        NTV2VPIDColorimetry override_color{}; NTV2VPIDLuminance override_luminance{};
        require(card.GetSDIOutVPIDTransferCharacteristics(override_enabled, override_transfer, channel) && !override_enabled,
            "AJA SDI transfer override must be disabled before exact output");
        require(card.GetSDIOutVPIDColorimetry(override_enabled, override_color, channel) && !override_enabled,
            "AJA SDI color override must be disabled before exact output");
        require(card.GetSDIOutVPIDLuminance(override_enabled, override_luminance, channel) && !override_enabled,
            "AJA SDI luminance override must be disabled before exact output");
        bidirectional = card.features().HasBiDirectionalSDI();
        require(card.GetTaskMode(task) && card.GetMode(channel, mode) && card.GetVideoFormat(video, channel)
            && card.GetFrameBufferFormat(channel, pixels) && card.GetVANCMode(vanc, channel) && card.GetVANCShiftMode(channel, vanc_shift)
            && card.IsChannelEnabled(channel, enabled) && card.GetConnections(routing)
            && card.GetNumberAudioChannels(channels, audio_system) && card.GetAudioRate(rate, audio_system)
            && card.GetAudioBufferSize(buffer, audio_system) && card.GetAudioLoopBack(loopback, audio_system) && card.GetEmbeddedAudioClock(audio_clock, audio_system)
            && card.GetSDIOutputAudioSystem(channel, embed) && card.GetSDIOutputStandard(channel, standard)
            && card.GetVPIDTransferCharacteristics(transfer, channel) && card.GetVPIDColorimetry(color, channel)
            && card.GetVPIDLuminance(luminance, channel) && card.GetSDIOut3GEnable(channel, three_g)
            && card.GetSDIOut3GbEnable(channel, three_g_b)
            && (!bidirectional || card.GetSDITransmitEnable(channel, transmit)), "AJA pre-mutation configuration snapshot failed");
        captured = true;
    }
    bool restore(CNTV2Card& card) noexcept {
        if (!captured) return true;
        bool ok = true;
        try {
            // Evaluate every restoration even after a preceding failure.
            ok = card.SetVideoFormat(video, false, false, channel) && ok;
            ok = card.SetFrameBufferFormat(channel, pixels) && ok;
            ok = card.SetVPIDTransferCharacteristics(transfer, channel) && ok;
            ok = card.SetVPIDColorimetry(color, channel) && ok;
            ok = card.SetVPIDLuminance(luminance, channel) && ok;
            ok = card.SetVANCMode(vanc, channel) && ok;
            ok = card.SetVANCShiftMode(channel, vanc_shift) && ok;
            ok = card.SetMode(channel, mode) && ok;
            ok = card.SetNumberAudioChannels(channels, audio_system) && ok;
            ok = card.SetAudioRate(rate, audio_system) && ok;
            ok = card.SetAudioBufferSize(buffer, audio_system) && ok;
            ok = card.SetAudioLoopBack(loopback, audio_system) && ok;
            ok = card.SetEmbeddedAudioClock(audio_clock, audio_system) && ok;
            ok = card.SetSDIOutputAudioSystem(channel, embed) && ok;
            ok = card.SetSDIOutputStandard(channel, standard) && ok;
            ok = card.SetSDIOut3GEnable(channel, three_g) && ok;
            ok = card.SetSDIOut3GbEnable(channel, three_g_b) && ok;
            if (bidirectional) ok = card.SetSDITransmitEnable(channel, transmit) && ok;
            ok = card.ApplySignalRoute(routing, true) && ok;
            ok = (enabled ? card.EnableChannel(channel) : card.DisableChannel(channel)) && ok;
            ok = card.SetTaskMode(task) && ok;
        } catch (...) { ok = false; }
        return ok;
    }
};
struct Session {
    CNTV2Card card;
    MdAjaRequest request{};
    MdAjaDevice identity{};
    NTV2FormatDescriptor format;
    SavedConfiguration saved;
    std::mutex mutex;
    std::condition_variable wake;
    std::deque<Frame> queued;
    std::deque<uint64_t> inflight;
    std::deque<MdAjaEvent> events;
    std::thread worker;
    std::atomic<bool> stop{false}, start{false};
    bool acquired = false, configured = false, subscribed = false, running = false;
    bool reference_observed = false, last_lock = false;
    uint64_t last_active = 0, scheduled = 0, last_scheduled = 0, unresolved = 0;
    uint32_t embedded_channels = 0, previous_drops = 0;
    std::string fault;
    void event(uint32_t kind, uint64_t frame = 0, uint64_t ticks = 0) {
        std::lock_guard<std::mutex> guard(mutex);
        if (events.size() >= request.max_frames * 4 + 8) {
            fault = "AJA callback event queue overflow"; stop = true; return;
        }
        events.push_back({kind, 0, frame, ticks});
    }
    void configure() {
        require(request.mode < format_count && request.channels && request.channels <= 16
            && request.max_frames >= 3 && request.max_frames <= 64 && request.preroll >= 2
            && request.preroll < request.max_frames && request.ancillary <= 1 && request.require_reference <= 1,
            "AJA request exceeds supported exact bounds");
        require((identity.mode_mask & (1u << request.mode)) != 0, "AJA video mode is unsupported");
        require(!request.require_reference || reference_lock(card) == 1, "AJA external reference is not locked");
        require(card.AcquireStreamForApplication(owner_signature, int32_t(GetCurrentProcessId())), "AJA device is owned by another application");
        acquired = true;
        saved.capture(card);
        embedded_channels = card.features().GetMaxAudioChannels() >= 16 ? 16 : 8;
        require(request.channels <= embedded_channels, "AJA embedded Audio channel extent unavailable");
        require(card.SetTaskMode(NTV2_OEM_TASKS) && card.SetVideoFormat(formats[request.mode], false, false, channel)
            && card.SetFrameBufferFormat(channel, NTV2_FBF_10BIT_YCBCR, false)
            && card.SetVPIDTransferCharacteristics(NTV2_VPID_TC_SDR_TV, channel)
            && card.SetVPIDColorimetry(NTV2_VPID_Color_Rec709, channel)
            && card.SetVPIDLuminance(NTV2_VPID_Luminance_YCbCr, channel)
            && card.SetVANCMode(request.ancillary ? NTV2_VANCMODE_TALL : NTV2_VANCMODE_OFF, channel)
            && card.SetVANCShiftMode(channel, NTV2_VANCDATA_NORMAL) && card.SetMode(channel, NTV2_MODE_DISPLAY)
            && card.EnableChannel(channel) && card.SetAudioRate(NTV2_AUDIO_48K, audio_system)
            && card.SetNumberAudioChannels(embedded_channels, audio_system)
            && card.SetAudioBufferSize(NTV2_AUDIO_BUFFER_BIG, audio_system)
            && card.SetAudioLoopBack(NTV2_AUDIO_LOOPBACK_OFF, audio_system)
            && card.SetSDIOutputAudioSystem(channel, audio_system)
            && card.SetSDIOutputStandard(channel, GetNTV2StandardFromVideoFormat(formats[request.mode]))
            && card.SetSDIOut3GEnable(channel, request.mode >= 5 && request.mode <= 7)
            && card.SetSDIOut3GbEnable(channel, false)
            && (!saved.bidirectional || card.SetSDITransmitEnable(channel, true))
            && card.Connect(GetSDIOutputInputXpt(channel, false), GetFrameStoreOutputXptFromChannel(channel, false, false), true),
            "AJA exact video/audio/routing configuration failed");
        NTV2VideoFormat read_video{}; NTV2FrameBufferFormat read_pixels{}; NTV2AudioRate read_rate{};
        ULWord read_channels = 0; NTV2VPIDTransferCharacteristics read_transfer{}; NTV2VPIDColorimetry read_color{};
        require(card.GetVideoFormat(read_video, channel) && read_video == formats[request.mode]
            && card.GetFrameBufferFormat(channel, read_pixels) && read_pixels == NTV2_FBF_10BIT_YCBCR
            && card.GetAudioRate(read_rate, audio_system) && read_rate == NTV2_AUDIO_48K
            && card.GetNumberAudioChannels(read_channels, audio_system) && read_channels == embedded_channels
            && card.GetVPIDTransferCharacteristics(read_transfer, channel) && read_transfer == NTV2_VPID_TC_SDR_TV
            && card.GetVPIDColorimetry(read_color, channel) && read_color == NTV2_VPID_Color_Rec709,
            "AJA live configuration readback mismatch");
        format = NTV2FormatDescriptor(formats[request.mode], NTV2_FBF_10BIT_YCBCR,
            request.ancillary ? NTV2_VANCMODE_TALL : NTV2_VANCMODE_OFF);
        require(format.GetTotalBytes() != 0 && (format.GetFirstActiveLine() != 0) == bool(request.ancillary), "AJA picture/VANC DMA extent invalid");
        subscribed = true; require(card.SubscribeOutputVerticalEvent(channel), "AJA output interrupt subscription failed");
        configured = true;
        require(card.AutoCirculateInitForOutput(channel, UWord(request.max_frames + 1), audio_system,
            0), "AJA AutoCirculate output initialization failed");
        configured = true;
        const int32_t initial_lock = reference_lock(card);
        if (initial_lock >= 0) {
            last_lock = initial_lock == 1; reference_observed = true;
            event(last_lock ? 5 : 6);
        }
        worker = std::thread([this] { run(); });
    }
    void run() noexcept {
        try {
            uint64_t transferred = 0;
            while (!stop) {
                const int32_t locked = reference_lock(card);
                if (locked >= 0 && (!reference_observed || last_lock != (locked == 1))) {
                    last_lock = locked == 1; reference_observed = true; event(last_lock ? 5 : 6);
                }
                require(!request.require_reference || locked == 1, "AJA external reference lock lost");
                NTV2VideoFormat current_video{}; NTV2FrameBufferFormat current_pixels{};
                NTV2OutputCrosspointID current_route{};
                require(card.GetVideoFormat(current_video, channel) && current_video == formats[request.mode]
                    && card.GetFrameBufferFormat(channel, current_pixels) && current_pixels == NTV2_FBF_10BIT_YCBCR
                    && card.GetConnectedOutput(GetSDIOutputInputXpt(channel, false), current_route)
                    && current_route == GetFrameStoreOutputXptFromChannel(channel, false, false),
                    "AJA active device profile or route changed");
                AUTOCIRCULATE_STATUS status;
                require(card.AutoCirculateGetStatus(channel, status), "AJA device status lost");
                if (status.GetDroppedFrameCount() != previous_drops) {
                    previous_drops = status.GetDroppedFrameCount();
                    event(3, last_active ? last_active - 1 : 0);
                    throw std::runtime_error("AJA hardware reports dropped/repeated output frame");
                }
                if (running && status.GetActiveFrame() >= 0) {
                    FRAME_STAMP stamp;
                    require(card.AutoCirculateGetFrameStamp(channel, ULWord(status.GetActiveFrame()), stamp), "AJA hardware frame stamp failed");
                    const uint64_t active = stamp.acCurrentUserCookie;
                    if (active && active != last_active) {
                        std::deque<MdAjaEvent> observed;
                        {
                            std::lock_guard<std::mutex> guard(mutex);
                            while (!inflight.empty() && inflight.front() < active) {
                                const uint64_t previous = inflight.front(); inflight.pop_front(); --unresolved;
                                // Only a frame observed actively on the jack can be
                                // completed. Missed observations become drops.
                                observed.push_back({previous == last_active ? 1u : 3u, 0, previous - 1,
                                    previous == last_active ? stamp.acAudioClockCurrentTime : 0});
                            }
                        }
                        for (const auto& item : observed) event(item.kind, item.frame, item.ticks);
                        last_active = active;
                    }
                }
                if (status.CanAcceptMoreOutputFrames()) {
                    Frame frame;
                    {
                        std::lock_guard<std::mutex> guard(mutex);
                        if (!queued.empty()) { frame = std::move(queued.front()); queued.pop_front(); }
                    }
                    if (!frame.video.empty()) {
                        AUTOCIRCULATE_TRANSFER transfer;
                        require(transfer.SetVideoBuffer(frame.video.data(), ULWord(frame.video.size() * 4)), "AJA video DMA setup failed");
                        require(transfer.SetAudioBuffer(frame.audio.data(), ULWord(frame.audio.size() * 4)), "AJA Audio DMA setup failed");
                        transfer.acInUserCookie = frame.index + 1;
                        require(card.AutoCirculateTransfer(channel, transfer), "AJA atomic picture/Audio/ANC DMA failed");
                        { std::lock_guard<std::mutex> guard(mutex); inflight.push_back(frame.index + 1); }
                        ++transferred;
                    }
                }
                if (!running && start && transferred >= request.preroll) {
                    require(card.AutoCirculateStart(channel), "AJA scheduled start failed"); running = true;
                }
                if (running) require(card.WaitForOutputVerticalInterrupt(channel), "AJA output vertical interrupt lost");
                else { std::unique_lock<std::mutex> lock(mutex); wake.wait_for(lock, std::chrono::milliseconds(1)); }
            }
        } catch (const std::exception& ex) {
            { std::lock_guard<std::mutex> guard(mutex); fault = ex.what(); }
            event(7); stop = true;
        } catch (...) {
            { std::lock_guard<std::mutex> guard(mutex); fault = "AJA native worker exception"; }
            event(7); stop = true;
        }
    }
    void shutdown(MdAjaShutdown& receipt) noexcept {
        stop = true; wake.notify_all(); receipt.requested = 1;
        try {
            if (worker.joinable()) worker.join();
            receipt.worker_joined = 1;
            bool stopped = !configured || card.AutoCirculateStop(channel);
            if (configured && stopped) { AUTOCIRCULATE_STATUS status; stopped = card.AutoCirculateGetStatus(channel, status) && status.IsStopped(); }
            receipt.stopped = stopped ? 1 : 0;
            const bool restored = saved.restore(card);
            const bool unsubscribed = !subscribed || card.UnsubscribeOutputVerticalEvent(channel);
            const bool released = !acquired || card.ReleaseStreamForApplication(owner_signature, int32_t(GetCurrentProcessId()));
            const bool closed = !card.IsOpen() || card.Close();
            receipt.released = released && closed ? 1 : 0;
            if (!stopped || !restored || !unsubscribed || !released || !closed)
                receipt.outstanding_resources = 1;
            if (!stopped) receipt.outstanding_frames = unresolved;
            if (!fault.empty()) text(receipt.error, sizeof(receipt.error), fault);
            else if (receipt.outstanding_resources) text(receipt.error, sizeof(receipt.error), "AJA stop/configuration restoration/release incomplete");
        } catch (...) { receipt.outstanding_resources = 1; text(receipt.error, sizeof(receipt.error), "AJA consuming shutdown exception"); }
    }
};
void capture_preflight(CNTV2Card& card, uint32_t mode) {
    require(mode < format_count && card.features().CanDoCapture() && card.features().GetNumVideoInputs()
        && card.features().CanDoFrameBufferFormat(NTV2_FBF_10BIT_YCBCR)
        && card.features().CanDoVideoFormat(formats[mode]) && !card.features().CanDo2110(),
        "AJA independent SDI capture mode unavailable");
    const NTV2FormatDescriptor descriptor(formats[mode], NTV2_FBF_10BIT_YCBCR, NTV2_VANCMODE_TALL);
    (void)vanc_geometry(descriptor);
}
struct CaptureSession {
    CNTV2Card card;
    SavedConfiguration saved;
    NTV2FormatDescriptor format;
    std::vector<uint32_t> raster;
    uint32_t mode = 0;
    bool acquired = false, initialized = false, subscribed = false, started = false;
    uint64_t last_ticks = 0;
    void configure(uint32_t requested_mode, uint32_t capacity) {
        mode = requested_mode;
        capture_preflight(card, mode);
        require(capacity >= 3 && capacity <= 64, "AJA capture ring exceeds bounds");
        require(card.AcquireStreamForApplication(owner_signature, int32_t(GetCurrentProcessId())), "AJA capture device already owned");
        acquired = true; saved.capture(card);
        require(card.SetTaskMode(NTV2_OEM_TASKS) && card.SetVideoFormat(formats[mode], false, false, channel)
            && card.SetMode(channel, NTV2_MODE_CAPTURE) && card.SetFrameBufferFormat(channel, NTV2_FBF_10BIT_YCBCR, false)
            && card.SetVANCMode(NTV2_VANCMODE_TALL, channel) && card.SetVANCShiftMode(channel, NTV2_VANCDATA_NORMAL)
            && card.EnableChannel(channel) && (!saved.bidirectional || card.SetSDITransmitEnable(channel, false))
            && card.Connect(GetFrameStoreInputXptFromChannel(channel), GetSDIInputOutputXptFromChannel(channel), true),
            "AJA independent SDI/VANC capture configuration failed");
        NTV2VANCMode vanc{}; NTV2FrameBufferFormat pixels{}; NTV2Mode read_mode{};
        require(card.GetVANCMode(vanc, channel) && vanc == NTV2_VANCMODE_TALL
            && card.GetFrameBufferFormat(channel, pixels) && pixels == NTV2_FBF_10BIT_YCBCR
            && card.GetMode(channel, read_mode) && read_mode == NTV2_MODE_CAPTURE, "AJA capture profile readback mismatch");
        format = NTV2FormatDescriptor(formats[mode], NTV2_FBF_10BIT_YCBCR, NTV2_VANCMODE_TALL);
        raster.resize(format.GetTotalBytes() / 4);
        subscribed = true;
        require(card.SubscribeInputVerticalEvent(channel), "AJA capture interrupt subscription failed");
        initialized = true;
        require(card.AutoCirculateInitForInput(channel, UWord(capacity), NTV2_AUDIOSYSTEM_INVALID, 0), "AJA independent capture initialization failed");
    }
    bool poll(MdAjaWireFrame& result) {
        if (!started) return false;
        require(initialized && card.GetInputVideoFormat(NTV2_INPUTSOURCE_SDI1) == formats[mode], "AJA capture signal absent or wrong mode");
        NTV2OutputCrosspointID route{}; NTV2VANCMode vanc{};
        require(card.GetConnectedOutput(GetFrameStoreInputXptFromChannel(channel), route)
            && route == GetSDIInputOutputXptFromChannel(channel)
            && card.GetVANCMode(vanc, channel) && vanc == NTV2_VANCMODE_TALL,
            "AJA capture route or VANC profile changed");
        AUTOCIRCULATE_STATUS status;
        require(card.AutoCirculateGetStatus(channel, status) && status.IsRunning()
            && status.GetDroppedFrameCount() == 0, "AJA independent capture stopped or overran");
        if (!status.HasAvailableInputFrame()) return false;
        AUTOCIRCULATE_TRANSFER transfer;
        require(transfer.SetVideoBuffer(raster.data(), ULWord(raster.size() * 4))
            && card.AutoCirculateTransfer(channel, transfer), "AJA independent capture DMA failed");
        const uint64_t ticks = transfer.GetFrameInfo().acAudioClockTimeStamp;
        require(ticks != 0 && ticks > last_ticks, "AJA captured hardware clock is absent or nonmonotonic");
        last_ticks = ticks;
        const auto packets = mondrian_aja::read_vanc_raster(raster, vanc_geometry(format));
        result = MdAjaWireFrame{};
        result.capture_ticks = ticks; result.count = uint32_t(packets.size());
        for (size_t n = 0; n < packets.size(); ++n) {
            auto& out = result.packets[n]; const auto& in = packets[n];
            out.line = in.line; out.offset = in.offset; out.count = uint32_t(in.words.size());
            std::copy(in.words.begin(), in.words.end(), out.words);
        }
        return true;
    }
    void shutdown(MdAjaShutdown& receipt) noexcept {
        receipt.requested = 1; receipt.worker_joined = 1; // synchronous owner has no callback worker
        try {
            bool stopped = !initialized || card.AutoCirculateStop(channel);
            if (initialized && stopped) { AUTOCIRCULATE_STATUS status; stopped = card.AutoCirculateGetStatus(channel, status) && status.IsStopped(); }
            receipt.stopped = stopped ? 1 : 0;
            const bool restored = saved.restore(card);
            const bool unsubscribed = !subscribed || card.UnsubscribeInputVerticalEvent(channel);
            const bool released = !acquired || card.ReleaseStreamForApplication(owner_signature, int32_t(GetCurrentProcessId()));
            const bool closed = !card.IsOpen() || card.Close();
            receipt.released = released && closed ? 1 : 0;
            if (!stopped || !restored || !unsubscribed || !released || !closed) {
                receipt.outstanding_resources = 1;
                text(receipt.error, sizeof(receipt.error), "AJA capture consuming shutdown incomplete");
            }
        } catch (...) { receipt.outstanding_resources = 1; text(receipt.error, sizeof(receipt.error), "AJA capture shutdown exception"); }
    }
};
} // namespace

uint32_t md_aja_abi_version() { return 1; }
int32_t md_aja_sdk_version(char* out, uint32_t capacity) {
    return boundary([&] { require(out && capacity >= 32, "SDK version buffer too small");
        text(out, capacity, std::to_string(AJA_NTV2_SDK_VERSION_MAJOR) + "." + std::to_string(AJA_NTV2_SDK_VERSION_MINOR)
            + "." + std::to_string(AJA_NTV2_SDK_VERSION_POINT) + "." + std::to_string(AJA_NTV2_SDK_BUILD_NUMBER)); }, out, capacity);
}
int32_t md_aja_discover(MdAjaDevice* out, uint32_t capacity, uint32_t* count, char* error, uint32_t size) {
    return boundary([&] {
        require(out && count && capacity <= 64, "invalid AJA discovery buffer"); *count = 0;
        const auto devices = CNTV2DeviceScanner::GetDeviceInfoList();
        require(devices.size() <= capacity, "AJA inventory exceeds fixed device bound");
        for (const auto& info : devices) {
            require(info.deviceIndex <= UINT16_MAX, "AJA device index exceeds SDK bounds");
            CNTV2Card card; require(card.Open(UWord(info.deviceIndex)), "AJA inventory device open failed");
            const auto device = describe(card);
            if (device.mode_mask) out[(*count)++] = device;
        }
    }, error, size);
}
int32_t md_aja_reference(uint64_t serial, uint64_t generation, int32_t* locked, char* error, uint32_t size) {
    return boundary([&] { require(locked != nullptr, "null lock status"); CNTV2Card card; open_exact(card, serial, generation); *locked = reference_lock(card); }, error, size);
}
int32_t md_aja_open(uint64_t serial, uint64_t generation, const MdAjaRequest* request, void** out, char* error, uint32_t size) {
    if (out) *out = nullptr;
    return boundary([&] {
        require(request && out, "null AJA open request");
        auto session = std::make_unique<Session>();
        session->request = *request;
        Session* owner = session.release(); *out = owner;
        open_exact(owner->card, serial, generation);
        owner->identity = describe(owner->card);
        owner->configure();
    }, error, size);
}
int32_t md_aja_schedule_vanc(void* raw, uint64_t index, const uint8_t* video, uint32_t bytes, uint32_t row,
    const int32_t* audio, uint32_t samples, const MdAjaWirePacket* packets, uint32_t packet_count, char* error, uint32_t size) {
    auto* session = static_cast<Session*>(raw);
    if (!session) { text(error, size, "null AJA Session"); return -1; }
    bool full = false;
    const int32_t result = boundary([&] {
        {
            std::lock_guard<std::mutex> guard(session->mutex);
            if (session->unresolved >= session->request.max_frames) { full = true; return; }
        }
        require(!session->stop && video && audio && index != UINT64_MAX, "AJA Session stopped or malformed frame");
        require(packet_count <= 64 && (!packet_count || (packets && session->request.ancillary)), "AJA VANC contract unavailable");
        require(session->configured && session->request.channels != 0, "AJA Session never finished opening");
        const uint32_t height = session->format.GetVisibleRasterHeight();
        const uint32_t stride = session->format.GetBytesPerRow();
        require(row >= stride && uint64_t(row) * height == bytes && samples % session->request.channels == 0,
            "AJA picture/Audio payload extent mismatch");
        const uint32_t audio_frames = samples / session->request.channels;
        require(audio_frames <= 4096, "AJA Audio frame exceeds fixed DMA bound");
        Frame frame; frame.index = index; frame.video.resize(session->format.GetTotalBytes() / 4);
        for (uint32_t line = 0; line < height; ++line)
            std::memcpy(reinterpret_cast<uint8_t*>(frame.video.data()) + size_t(line + session->format.GetFirstActiveLine()) * stride, video + size_t(line) * row, stride);
        if (session->request.ancillary) {
            std::vector<mondrian_aja::VancPacket> inventory;
            for (uint32_t n = 0; n < packet_count; ++n) {
                const auto& packet = packets[n];
                require(packet.count >= 7 && packet.count <= 262 && packet.reserved == 0, "AJA raw VANC packet extent invalid");
                inventory.push_back({packet.line, packet.offset, {packet.words, packet.words + packet.count}});
            }
            mondrian_aja::write_vanc_raster(frame.video, vanc_geometry(session->format), inventory);
        }
        frame.audio.assign(size_t(audio_frames) * session->embedded_channels, 0);
        for (uint32_t n = 0; n < audio_frames; ++n) for (uint32_t ch = 0; ch < session->request.channels; ++ch) {
            const int32_t sample = audio[size_t(n) * session->request.channels + ch];
            require(sample >= -8388608 && sample <= 8388607, "AJA signed 24-bit Audio sample is invalid");
            frame.audio[size_t(n) * session->embedded_channels + ch] = uint32_t(sample) << 8;
        }
        {
            std::lock_guard<std::mutex> guard(session->mutex);
            require(!session->stop && session->fault.empty(), "AJA owner failed during frame packing");
            require(session->unresolved < session->request.max_frames, "AJA concurrent frame queue bound changed");
            require(!session->scheduled || index == session->last_scheduled + 1, "AJA frame sequence is discontinuous");
            session->queued.push_back(std::move(frame)); ++session->unresolved; ++session->scheduled; session->last_scheduled = index;
        }
        session->wake.notify_all();
    }, error, size);
    return result == 0 && full ? 1 : result;
}
int32_t md_aja_schedule(void* raw, uint64_t index, const uint8_t* video, uint32_t bytes, uint32_t row,
    const int32_t* audio, uint32_t samples, const MdAjaPacket* packets, uint32_t count, char* error, uint32_t size) {
    (void)packets;
    if (count) { text(error, size, "legacy byte-only ANC ABI cannot preserve canonical words"); return -1; }
    return md_aja_schedule_vanc(raw, index, video, bytes, row, audio, samples, nullptr, 0, error, size);
}
int32_t md_aja_start(void* raw, char* error, uint32_t size) {
    return boundary([&] { auto* session = static_cast<Session*>(raw); require(session && !session->stop, "AJA Session unavailable");
        { std::lock_guard<std::mutex> guard(session->mutex); require(session->scheduled >= session->request.preroll, "AJA preroll incomplete"); }
        session->start = true; session->wake.notify_all(); }, error, size);
}
int32_t md_aja_poll(void* raw, MdAjaEvent* event, char* error, uint32_t size) {
    int32_t available = 0;
    const int32_t result = boundary([&] { auto* session = static_cast<Session*>(raw); require(session && event, "null AJA poll request");
        std::lock_guard<std::mutex> guard(session->mutex);
        if (!session->events.empty()) { *event = session->events.front(); session->events.pop_front(); available = 1; }
        else require(session->fault.empty(), session->fault.c_str()); }, error, size);
    return result < 0 ? result : available;
}
int32_t md_aja_request_stop(void* raw) {
    auto* session = static_cast<Session*>(raw); if (!session) return -1;
    session->stop = true; session->wake.notify_all(); return 0;
}
int32_t md_aja_shutdown(void* raw, MdAjaShutdown* receipt) {
    if (!raw || !receipt) return -1;
    *receipt = MdAjaShutdown{};
    auto* session = static_cast<Session*>(raw); session->shutdown(*receipt);
    // Never destroy a still-joinable std::thread (e.g. an exceptional join).
    if (session->worker.joinable()) { receipt->outstanding_resources = 1; return -1; }
    delete session; return 0;
}
int32_t md_aja_capture_preflight(uint64_t serial, uint64_t generation, uint32_t mode, uint32_t line, uint32_t offset, char* error, uint32_t size) {
    return boundary([&] {
        CNTV2Card card; open_exact(card, serial, generation); capture_preflight(card, mode);
        const auto geometry = vanc_geometry(NTV2FormatDescriptor(formats[mode], NTV2_FBF_10BIT_YCBCR, NTV2_VANCMODE_TALL));
        require(std::find(geometry.lines.begin(), geometry.lines.end(), line) != geometry.lines.end()
            && uint64_t(offset) + 39 <= geometry.width, "AJA marker position is outside the physical VANC raster");
        require(card.GetInputVideoFormat(NTV2_INPUTSOURCE_SDI1) == formats[mode], "AJA independent receiver has no exact live SDI input route");
    }, error, size);
}
int32_t md_aja_capture_open(uint64_t serial, uint64_t generation, uint32_t mode, uint32_t capacity,
    void** out, char* error, uint32_t size) {
    if (out) *out = nullptr;
    return boundary([&] {
        require(out != nullptr, "null AJA capture owner");
        auto session = std::make_unique<CaptureSession>();
        auto* owner = session.release(); *out = owner;
        open_exact(owner->card, serial, generation); owner->configure(mode, capacity);
    }, error, size);
}
int32_t md_aja_capture_poll(void* raw, MdAjaWireFrame* frame, char* error, uint32_t size) {
    bool ready = false;
    const int32_t result = boundary([&] {
        require(raw && frame, "null AJA capture poll"); ready = static_cast<CaptureSession*>(raw)->poll(*frame);
    }, error, size);
    return result == 0 ? int32_t(ready) : result;
}
int32_t md_aja_capture_start(void* raw, char* error, uint32_t size) {
    return boundary([&] {
        require(raw != nullptr, "null AJA capture start");
        auto* session = static_cast<CaptureSession*>(raw);
        require(session->initialized && !session->started && session->card.AutoCirculateStart(channel), "AJA independent capture start failed");
        session->started = true;
    }, error, size);
}
int32_t md_aja_capture_shutdown(void* raw, MdAjaShutdown* receipt) {
    if (!raw || !receipt) return -1;
    *receipt = MdAjaShutdown{};
    auto* session = static_cast<CaptureSession*>(raw); session->shutdown(*receipt); delete session; return 0;
}
