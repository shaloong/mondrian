// SPDX-License-Identifier: MIT
// Independently implemented COM owner, using unmodified BMD-licensed API 12.0
// IDL redistributed by OBS at the commit recorded in vendor/README.md.
#include "bridge.h"
#include "vanc_lines.h"
#include <Windows.h>
#include <Unknwn.h>
#include <objbase.h>
#include "DeckLinkAPI.h"
#include "DeckLinkAPIVersion.h"
#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstring>
#include <deque>
#include <functional>
#include <future>
#include <limits>
#include <map>
#include <memory>
#include <mutex>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

namespace {
struct Failure : std::runtime_error {
    int32_t code;
    Failure(int32_t value, const std::string& detail) : std::runtime_error(detail), code(value) {}
};
void require(bool value, const char* detail, int32_t code = -5) { if (!value) throw Failure(code, detail); }
void hr(HRESULT value, const char* operation) {
    if (SUCCEEDED(value)) return;
    std::ostringstream message; message << operation << " HRESULT=0x" << std::hex << uint32_t(value);
    throw Failure(value == REGDB_E_CLASSNOTREG ? -2 : value == E_NOINTERFACE ? -4 : -1, message.str());
}
void text(char* buffer, uint32_t size, const std::string& value) {
    if (!buffer || size == 0) return;
    const size_t count = std::min<size_t>(value.size(), size - 1);
    std::memcpy(buffer, value.data(), count); buffer[count] = 0;
}
void text(char* buffer, uint32_t size, const char* value) noexcept {
    if (!buffer || size == 0 || !value) return;
    const size_t count = std::min<size_t>(std::strlen(value), size - 1);
    std::memcpy(buffer, value, count); buffer[count] = 0;
}
template<class F> int32_t boundary(F&& body, char* error, uint32_t bytes) noexcept {
    try { return body(); }
    catch (const Failure& value) { text(error, bytes, value.what()); return value.code; }
    catch (const std::exception& value) { text(error, bytes, value.what()); return -1; }
    catch (...) { text(error, bytes, "unknown DeckLink bridge exception"); return -1; }
}
template<class T> struct Com {
    T* value = nullptr;
    Com() = default;
    explicit Com(T* ptr) : value(ptr) {}
    Com(const Com&) = delete; Com& operator=(const Com&) = delete;
    Com(Com&& other) noexcept : value(other.value) { other.value = nullptr; }
    Com& operator=(Com&& other) noexcept { reset(); value = other.value; other.value = nullptr; return *this; }
    ~Com() { reset(); }
    void reset() noexcept { if (value) value->Release(); value = nullptr; }
    T* operator->() const { return value; }
    explicit operator bool() const { return value != nullptr; }
    T** put() { require(!value, "COM result already owned"); return &value; }
};
template<class T, class U> Com<T> query(U* object) {
    Com<T> result; hr(object->QueryInterface(__uuidof(T), reinterpret_cast<void**>(result.put())), "QueryInterface API 12.0"); return result;
}
template<class T> Com<T> create(REFCLSID clsid) {
    Com<T> result; hr(CoCreateInstance(clsid, nullptr, CLSCTX_INPROC_SERVER, __uuidof(T),
        reinterpret_cast<void**>(result.put())), "CoCreateInstance DeckLink (Desktop Video driver required)"); return result;
}
struct Mta {
    Mta() { hr(CoInitializeEx(nullptr, COINIT_MULTITHREADED), "CoInitializeEx MTA"); }
    ~Mta() { CoUninitialize(); }
};
template<class F> auto on_mta(F body) {
    return std::async(std::launch::async, [body] { Mta mta; return body(); }).get();
}
struct Bstr {
    BSTR value = nullptr;
    ~Bstr() { SysFreeString(value); }
    std::string utf8() const {
        require(value && SysStringLen(value) > 0 && SysStringLen(value) <= 4096, "invalid DeckLink BSTR");
        const int count = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value, int(SysStringLen(value)), nullptr, 0, nullptr, nullptr);
        require(count > 0, "DeckLink UTF-16 conversion failed");
        std::string result(size_t(count), '\0');
        require(WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value, int(SysStringLen(value)), result.data(), count, nullptr, nullptr) == count,
            "DeckLink UTF-8 conversion changed"); return result;
    }
};
constexpr BMDDisplayMode modes[] = {bmdModeHD1080p2398, bmdModeHD1080p24, bmdModeHD1080p25,
    bmdModeHD1080p2997, bmdModeHD1080p30, bmdModeHD1080p50, bmdModeHD1080p5994, bmdModeHD1080p6000,
    bmdModeHD720p50, bmdModeHD720p5994, bmdModeHD720p60};
constexpr size_t mode_count = sizeof(modes) / sizeof(modes[0]);
constexpr uint32_t rate_num[] = {24000, 24, 25, 30000, 30, 50, 60000, 60, 50, 60000, 60};
constexpr uint32_t rate_den[] = {1001, 1, 1, 1001, 1, 1, 1001, 1, 1, 1001, 1};
uint64_t fingerprint(const std::string& value) {
    uint64_t hash = 14695981039346656037ULL;
    for (const unsigned char byte : value) { hash ^= byte; hash *= 1099511628211ULL; }
    return hash ? hash : 1;
}
std::string driver_version() {
    auto api = create<IDeckLinkAPIInformation>(CLSID_CDeckLinkAPIInformation);
    Bstr version; hr(api->GetString(BMDDeckLinkAPIVersion, &version.value), "Get Desktop Video API version"); return version.utf8();
}
LONGLONG attribute(IDeckLinkProfileAttributes* attributes, BMDDeckLinkAttributeID id) {
    LONGLONG result = 0; hr(attributes->GetInt(id, &result), "Get physical DeckLink attribute"); return result;
}
bool flag(IDeckLinkProfileAttributes* attributes, BMDDeckLinkAttributeID id) {
    BOOL result = FALSE; hr(attributes->GetFlag(id, &result), "Get DeckLink capability flag"); return result != FALSE;
}
MdDeckLinkDevice describe(IDeckLink* device, const std::string& driver) {
    auto attrs = query<IDeckLinkProfileAttributes>(device);
    MdDeckLinkDevice result{};
    result.serial = uint64_t(attribute(attrs.value, BMDDeckLinkPersistentID));
    result.physical_group = uint64_t(attribute(attrs.value, BMDDeckLinkDeviceGroupID));
    const auto topology = attribute(attrs.value, BMDDeckLinkTopologicalID);
    require(result.serial && result.physical_group, "DeckLink stable physical identity unavailable", -4);
    const auto channels = attribute(attrs.value, BMDDeckLinkMaximumAudioChannels);
    require(channels >= 0 && channels <= 64, "invalid DeckLink audio capacity"); result.maximum_audio_channels = uint32_t(channels);
    const auto io = attribute(attrs.value, BMDDeckLinkVideoIOSupport);
    if ((io & bmdDeviceSupportsPlayback) && (attribute(attrs.value, BMDDeckLinkVideoOutputConnections) & bmdVideoConnectionSDI)) {
        auto output = query<IDeckLinkOutput>(device);
        for (uint32_t n = 0; n < mode_count; ++n) {
            BOOL supported = FALSE; BMDDisplayMode actual{};
            hr(output->DoesSupportVideoMode(bmdVideoConnectionSDI, modes[n], bmdFormat10BitYUV,
                bmdNoVideoOutputConversion, bmdSupportedVideoModeDefault, &actual, &supported), "probe exact SDI v210 output mode");
            if (supported && actual == modes[n]) result.mode_mask |= 1u << n;
        }
        // The API exposes raw VANC; actual allocation depends on an enabled
        // output mode and is checked during open. This is not wire evidence.
        result.flags |= 1;
    }
    if ((io & bmdDeviceSupportsCapture) && (attribute(attrs.value, BMDDeckLinkVideoInputConnections) & bmdVideoConnectionSDI)) {
        auto input = query<IDeckLinkInput>(device);
        for (uint32_t n = 0; n < mode_count; ++n) {
            BOOL supported = FALSE; BMDDisplayMode actual{};
            hr(input->DoesSupportVideoMode(bmdVideoConnectionSDI, modes[n], bmdFormat10BitYUV,
                bmdNoVideoInputConversion, bmdSupportedVideoModeDefault, &actual, &supported), "probe exact SDI v210 input mode");
            if (supported && actual == modes[n]) result.input_mode_mask |= 1u << n;
        }
    }
    if (flag(attrs.value, BMDDeckLinkHasReferenceInput)) result.flags |= 2;
    Bstr name; hr(device->GetDisplayName(&name.value), "Get DeckLink display name");
    text(result.name, sizeof(result.name), name.utf8()); text(result.driver, sizeof(result.driver), driver);
    result.generation = fingerprint(std::to_string(result.serial) + ":" + std::to_string(result.physical_group)
        + ":" + std::to_string(topology) + ":" + driver + ":" + std::to_string(result.mode_mask)
        + ":" + std::to_string(result.input_mode_mask) + ":" + std::to_string(result.flags)
        + ":" + std::to_string(attribute(attrs.value, BMDDeckLinkProfileID)));
    return result;
}
Com<IDeckLink> open_exact(uint64_t serial, uint64_t generation, MdDeckLinkDevice& identity) {
    require(serial && generation, "invalid DeckLink exact identity");
    const auto driver = driver_version(); auto iterator = create<IDeckLinkIterator>(CLSID_CDeckLinkIterator);
    for (uint32_t count = 0; count < 64; ++count) {
        Com<IDeckLink> device; const auto status = iterator->Next(device.put());
        if (status == S_FALSE) break; hr(status, "DeckLink iterator Next"); require(bool(device), "null DeckLink iterator member");
        auto attrs = query<IDeckLinkProfileAttributes>(device.value);
        if (uint64_t(attribute(attrs.value, BMDDeckLinkPersistentID)) != serial) continue;
        identity = describe(device.value, driver);
        require(identity.generation == generation, "DeckLink generation/profile changed", -3); return device;
    }
    throw Failure(-3, "exact DeckLink persistent device is unavailable");
}
int32_t reference(IDeckLinkOutput* output) {
    BMDReferenceStatus status{}; hr(output->GetReferenceStatus(&status), "Get hardware reference status");
    return status & bmdReferenceNotSupportedByHardware ? -1 : status & bmdReferenceLocked ? 1 : 0;
}
void idle(IDeckLink* device) {
    auto status = query<IDeckLinkStatus>(device); LONGLONG busy = 0;
    hr(status->GetInt(bmdDeckLinkStatusBusy, &busy), "Get DeckLink ownership status");
    require((busy & (bmdDeviceCaptureBusy | bmdDevicePlaybackBusy)) == 0, "DeckLink endpoint is owned by another session", -6);
}
struct Configuration {
    Com<IDeckLinkConfiguration> owner;
    struct Entry { BMDDeckLinkConfigurationID id; LONGLONG value; bool is_flag; };
    std::vector<Entry> saved;
    std::vector<std::pair<BMDDeckLinkConfigurationID, double>> saved_scalars;
    void scalar(BMDDeckLinkConfigurationID id, double value) {
        double previous = 0; hr(owner->GetFloat(id, &previous), "snapshot DeckLink configuration scalar");
        saved_scalars.emplace_back(id, previous); hr(owner->SetFloat(id, value), "set DeckLink configuration scalar");
        double actual = 0; hr(owner->GetFloat(id, &actual), "read back DeckLink configuration scalar");
        require(actual == value, "DeckLink scalar configuration readback mismatch", -1);
    }
    void integer(BMDDeckLinkConfigurationID id, LONGLONG value) {
        LONGLONG previous = 0; hr(owner->GetInt(id, &previous), "snapshot DeckLink configuration integer");
        saved.push_back({id, previous, false}); hr(owner->SetInt(id, value), "set DeckLink configuration integer");
        LONGLONG actual = 0; hr(owner->GetInt(id, &actual), "read back DeckLink configuration integer");
        require(actual == value, "DeckLink integer configuration readback mismatch", -1);
    }
    void boolean(BMDDeckLinkConfigurationID id, bool value) {
        BOOL previous = FALSE; hr(owner->GetFlag(id, &previous), "snapshot DeckLink configuration flag");
        saved.push_back({id, previous, true}); hr(owner->SetFlag(id, value ? TRUE : FALSE), "set DeckLink configuration flag");
        BOOL actual = FALSE; hr(owner->GetFlag(id, &actual), "read back DeckLink configuration flag");
        require(bool(actual) == value, "DeckLink flag configuration readback mismatch", -1);
    }
    bool restore() noexcept {
        bool success = true;
        for (auto item = saved_scalars.rbegin(); item != saved_scalars.rend(); ++item) {
            double actual = 0;
            const bool ok = SUCCEEDED(owner->SetFloat(item->first, item->second))
                && SUCCEEDED(owner->GetFloat(item->first, &actual)) && actual == item->second;
            success = success && ok;
        }
        saved_scalars.clear();
        for (auto item = saved.rbegin(); item != saved.rend(); ++item) {
            if (item->is_flag) {
                BOOL actual = FALSE;
                const bool ok = SUCCEEDED(owner->SetFlag(item->id, BOOL(item->value)))
                    && SUCCEEDED(owner->GetFlag(item->id, &actual)) && actual == item->value;
                success = success && ok;
            } else {
                LONGLONG actual = 0;
                const bool ok = SUCCEEDED(owner->SetInt(item->id, item->value))
                    && SUCCEEDED(owner->GetInt(item->id, &actual)) && actual == item->value;
                success = success && ok;
            }
        }
        saved.clear(); owner.reset(); return success;
    }
};
struct Completion { IDeckLinkVideoFrame* frame; BMDOutputFrameCompletionResult result; };
struct CallbackState {
    std::mutex mutex;
    std::deque<Completion> completed;
    std::deque<Com<IDeckLinkVideoInputFrame>> received;
    uint32_t capacity = 64;
    bool stopped = false, disabled = false, overflow = false, format_changed = false;
};
class Callbacks final : public IDeckLinkVideoOutputCallback, public IDeckLinkInputCallback {
    std::atomic<ULONG> references{1};
public:
    std::shared_ptr<CallbackState> state;
    explicit Callbacks(std::shared_ptr<CallbackState> value) : state(std::move(value)) {}
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** result) override {
        if (!result) return E_POINTER; *result = nullptr;
        if (iid == IID_IUnknown || iid == __uuidof(IDeckLinkVideoOutputCallback)) *result = static_cast<IDeckLinkVideoOutputCallback*>(this);
        else if (iid == __uuidof(IDeckLinkInputCallback)) *result = static_cast<IDeckLinkInputCallback*>(this);
        else return E_NOINTERFACE; AddRef(); return S_OK;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return ++references; }
    ULONG STDMETHODCALLTYPE Release() override { const auto count = --references; if (!count) delete this; return count; }
    HRESULT STDMETHODCALLTYPE ScheduledFrameCompleted(IDeckLinkVideoFrame* frame, BMDOutputFrameCompletionResult result) override {
        try {
            std::lock_guard<std::mutex> guard(state->mutex);
            if (state->disabled) return S_OK;
            if (!frame || state->completed.size() >= state->capacity) state->overflow = true;
            else state->completed.push_back({frame, result});
            return S_OK;
        } catch (...) { std::lock_guard<std::mutex> guard(state->mutex); state->overflow = true; return E_FAIL; }
    }
    HRESULT STDMETHODCALLTYPE ScheduledPlaybackHasStopped() override {
        try { std::lock_guard<std::mutex> guard(state->mutex); state->stopped = true; return S_OK; }
        catch (...) { return E_FAIL; }
    }
    HRESULT STDMETHODCALLTYPE VideoInputFormatChanged(BMDVideoInputFormatChangedEvents, IDeckLinkDisplayMode*, BMDDetectedVideoInputFormatFlags) override {
        try { std::lock_guard<std::mutex> guard(state->mutex); state->format_changed = true; return S_OK; }
        catch (...) { return E_FAIL; }
    }
    HRESULT STDMETHODCALLTYPE VideoInputFrameArrived(IDeckLinkVideoInputFrame* frame, IDeckLinkAudioInputPacket*) override {
        try {
            std::lock_guard<std::mutex> guard(state->mutex);
            if (state->disabled || !frame) return S_OK;
            if (state->received.size() >= state->capacity) { state->overflow = true; return E_FAIL; }
            frame->AddRef(); Com<IDeckLinkVideoInputFrame> retained(frame); state->received.push_back(std::move(retained)); return S_OK;
        } catch (...) { std::lock_guard<std::mutex> guard(state->mutex); state->overflow = true; return E_FAIL; }
    }
};
// All native interface calls and releases run in one dedicated MTA owner.
// Callback objects retain shared state independently of that owner; queued
// input frames are SDK AddRef leases, never references to output memory.
struct Owner {
    std::thread worker;
    std::mutex mutex;
    std::condition_variable wake;
    std::deque<std::function<void()>> commands;
    bool exit = false;
    std::atomic<bool> stop_signal{false};
    std::string fault;
    uint64_t serial, generation;
    MdDeckLinkDevice identity{};
    Com<IDeckLink> device;
    Configuration config;
    std::shared_ptr<CallbackState> state = std::make_shared<CallbackState>();
    Com<Callbacks> callback;
    uint32_t mode, capacity;
    bool configured = false, stop_requested = false, callbacks_closed = true, quarantined = false;
    Owner(uint64_t id, uint64_t gen, uint32_t video_mode, uint32_t bound) : serial(id), generation(gen), mode(video_mode), capacity(bound) {}
    virtual ~Owner() = default;
    virtual void initialize() = 0;
    virtual void pump() = 0;
    virtual void stop() = 0;
    virtual void close(MdDeckLinkShutdown&) noexcept = 0;
    void launch(std::chrono::milliseconds timeout = std::chrono::seconds(5)) {
        auto started = std::make_shared<std::promise<void>>(); auto result = started->get_future();
        worker = std::thread([this, started] {
            try {
                std::unique_ptr<Mta> apartment;
                try { apartment = std::make_unique<Mta>(); initialize(); started->set_value(); }
                catch (...) { started->set_exception(std::current_exception()); }
                for (;;) {
                    std::function<void()> command;
                    {
                        std::unique_lock<std::mutex> guard(mutex);
                        if (quarantined) wake.wait(guard, [&] { return exit || !commands.empty(); });
                        else wake.wait_for(guard, std::chrono::milliseconds(2), [&] { return exit || !commands.empty() || stop_signal.load(); });
                        if (exit && commands.empty()) break;
                        if (!commands.empty()) { command = std::move(commands.front()); commands.pop_front(); }
                    }
                    if (!quarantined && stop_signal.exchange(false)) {
                        try { stop(); }
                        catch (const std::exception& error) { fault = error.what(); }
                        catch (...) { fault = "DeckLink stop signal execution failed"; }
                    }
                    if (command) command();
                    if (configured && fault.empty()) {
                        try { pump(); }
                        catch (const std::exception& error) { fault = error.what(); }
                        catch (...) { fault = "DeckLink owner pump exception"; }
                    }
                }
            } catch (...) { try { started->set_exception(std::current_exception()); } catch (...) {} }
        });
        require(result.wait_for(timeout) == std::future_status::ready,
            "DeckLink initialization deadline expired; native owner remains retained", -1);
        result.get();
    }
    template<class F> auto invoke(F body) {
        using Result = decltype(body());
        auto task = std::make_shared<std::packaged_task<Result()>>(std::move(body)); auto result = task->get_future();
        {
            std::lock_guard<std::mutex> guard(mutex);
            require(worker.joinable() && !exit && commands.size() < 16, "DeckLink owner command queue closed/full", -6);
            commands.emplace_back([task] { (*task)(); });
        }
        wake.notify_one(); return result.get();
    }
    void common() {
        require(mode < mode_count && capacity >= 3 && capacity <= 64, "invalid DeckLink mode/queue bound");
        device = open_exact(serial, generation, identity); idle(device.value);
        config.owner = query<IDeckLinkConfiguration>(device.value);
        state->capacity = capacity * 2 + 4; callback = Com<Callbacks>(new Callbacks(state));
    }
    void signal_stop() noexcept { stop_signal.store(true); wake.notify_one(); }
    void healthy() { require(configured && fault.empty() && !stop_requested && !stop_signal.load(), fault.empty() ? "DeckLink owner is not accepting work" : fault.c_str(), -1); }
    void callback_fault() {
        std::lock_guard<std::mutex> guard(state->mutex);
        require(!state->overflow && !state->format_changed, "DeckLink callback overflow or unrequested signal format change", -1);
    }
    void quarantine(MdDeckLinkShutdown& receipt) noexcept {
        // Do not release COM, frame/audio leases or the apartment while driver
        // execution cannot be proven stopped. The parked owner is intentionally
        // process-retained and does no polling/SDK work after this failure.
        quarantined = true; configured = false;
        { std::lock_guard<std::mutex> guard(state->mutex); state->disabled = true; }
        receipt.requested = 1; receipt.worker_joined = 0; receipt.released = 0;
        receipt.outstanding_resources += 1;
        HMODULE pinned = nullptr;
        GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            reinterpret_cast<LPCWSTR>(&md_decklink_abi_version), &pinned);
        text(receipt.error, sizeof(receipt.error), "DeckLink execution did not quiesce; complete COM/callback/frame owner quarantined");
    }
    void finish_common(MdDeckLinkShutdown& receipt, bool success) noexcept {
        {
            std::lock_guard<std::mutex> guard(state->mutex); state->disabled = true;
            state->received.clear(); state->completed.clear();
        }
        callback.reset(); success = config.restore() && success; device.reset(); configured = false;
        receipt.released = success ? 1u : 0u; receipt.outstanding_resources = success ? 0u : 1u;
        if (!success) {
            // A driver that failed callback deregistration may retain COM
            // callback vtables. Pin our image before the Rust lease can drop;
            // the receipt remains failed and never claims release.
            HMODULE pinned = nullptr;
            GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
                reinterpret_cast<LPCWSTR>(&md_decklink_abi_version), &pinned);
            text(receipt.error, sizeof(receipt.error), "DeckLink stop/disable/configuration restoration failed; image pinned, release unqualified");
        }
    }
};
struct Output final : Owner {
    MdDeckLinkRequest request;
    Com<IDeckLinkOutput> output;
    struct Pending { Com<IDeckLinkMutableVideoFrame> frame; std::vector<int32_t> audio; uint64_t index; };
    std::map<IDeckLinkVideoFrame*, Pending> pending;
    std::deque<MdDeckLinkEvent> events;
    BMDTimeValue duration = 0; BMDTimeScale scale = 0;
    uint32_t width = 0, height = 0, row = 0;
    uint64_t scheduled = 0, last_index = 0, audio_cursor = 0;
    bool video_enabled = false, audio_enabled = false, callback_set = false, running = false, preroll = false;
    bool reference_seen = false; int32_t last_reference = -1;
    Output(uint64_t id, uint64_t gen, const MdDeckLinkRequest& value) : Owner(id, gen, value.mode, value.max_frames), request(value) {}
    void initialize() override {
        require((request.channels == 2 || request.channels == 8 || request.channels == 16) && request.ancillary <= 1
            && request.require_reference <= 1 && request.preroll >= 2 && request.preroll < request.max_frames,
            "invalid DeckLink request"); common();
        require((identity.mode_mask & (1u << mode)) && request.channels <= identity.maximum_audio_channels,
            "DeckLink output mode/audio channels unavailable", -3);
        require(!request.ancillary || (identity.flags & 1), "DeckLink raw ANC API unavailable", -4);
        output = query<IDeckLinkOutput>(device.value);
        auto attrs = query<IDeckLinkProfileAttributes>(device.value);
        require(request.preroll >= attribute(attrs.value, BMDDeckLinkMinimumPrerollFrames), "DeckLink minimum hardware preroll not met");
        Com<IDeckLinkDisplayMode> display; hr(output->GetDisplayMode(modes[mode], display.put()), "Get exact DeckLink output display mode");
        width = uint32_t(display->GetWidth()); height = uint32_t(display->GetHeight()); row = mondrian_decklink::row_bytes(width);
        require(width == (mode >= 8 ? 1280u : 1920u) && height == (mode >= 8 ? 720u : 1080u), "DeckLink raster readback mismatch", -1);
        require(display->GetFieldDominance() == bmdProgressiveFrame && (display->GetFlags() & bmdDisplayModeColorspaceRec709), "DeckLink exact progressive Rec709 mode unavailable", -4);
        hr(display->GetFrameRate(&duration, &scale), "Get exact output frame rate"); require(duration > 0 && scale > 0 && scale <= 60000, "invalid native frame cadence");
        require(duration <= 1001 && uint64_t(scale) * rate_den[mode] == uint64_t(duration) * rate_num[mode], "DeckLink frame-rate readback mismatch", -1);
        config.integer(bmdDeckLinkConfigVideoOutputConnection, bmdVideoConnectionSDI);
        config.integer(bmdDeckLinkConfigVideoOutputConversionMode, bmdNoVideoOutputConversion);
        config.integer(bmdDeckLinkConfigSDIOutputLinkConfiguration, bmdLinkConfigurationSingleLink);
        config.boolean(bmdDeckLinkConfig444SDIVideoOutput, false);
        config.boolean(bmdDeckLinkConfigRec2020Output, false);
        config.boolean(bmdDeckLinkConfigOutput1080pAsPsF, false);
        config.scalar(bmdDeckLinkConfigDigitalAudioOutputScale, 1.0);
        if (mode >= 5 && mode <= 7) config.boolean(bmdDeckLinkConfigSMPTELevelAOutput, true);
        last_reference = reference(output.value); reference_seen = true;
        require(!request.require_reference || last_reference == 1, "DeckLink external reference is not locked", -3);
        hr(output->SetScheduledFrameCompletionCallback(callback.value), "Set output callback"); callback_set = true; callbacks_closed = false;
        hr(output->EnableVideoOutput(modes[mode], request.ancillary ? bmdVideoOutputVANC : bmdVideoOutputFlagDefault), "Enable exact SDI video"); video_enabled = true;
        auto status = query<IDeckLinkStatus>(device.value);
        LONGLONG current_mode = 0, current_flags = 0;
        hr(status->GetInt(bmdDeckLinkStatusCurrentVideoOutputMode, &current_mode), "read current physical output mode");
        hr(status->GetInt(bmdDeckLinkStatusCurrentVideoOutputFlags, &current_flags), "read current physical output flags");
        // This status is BMDDeckLinkVideoStatusFlags (PsF/3D), not
        // BMDVideoOutputFlags (VANC/RP188). Progressive mono output has zero.
        require(current_mode == modes[mode] && current_flags == 0,
            "current physical output mode/flags differ from admitted request", -1);
        if (request.ancillary) {
            Com<IDeckLinkVideoFrameAncillary> ancillary;
            hr(output->CreateAncillaryData(bmdFormat10BitYUV, ancillary.put()), "admit native raw VANC allocation");
            require(bool(ancillary), "native raw VANC allocation returned null", -4);
        }
        hr(output->EnableAudioOutput(bmdAudioSampleRate48kHz, bmdAudioSampleType32bitInteger, request.channels,
            bmdAudioOutputStreamTimestamped), "Enable 48kHz timestamped SDI audio"); audio_enabled = true;
        hr(output->BeginAudioPreroll(), "Begin DeckLink audio preroll"); preroll = true; configured = true;
        events.push_back({last_reference == 1 ? 5u : 6u, 0, 0, 0});
    }
    int32_t schedule(uint64_t index, const uint8_t* video, uint32_t bytes, uint32_t stride,
        const int32_t* audio, uint32_t samples, const MdDeckLinkWirePacket* packets, uint32_t count) {
        healthy(); pump();
        require(video && audio && stride == row && uint64_t(bytes) == uint64_t(row) * height,
            "DeckLink v210 raster/stride mismatch");
        require(count <= 64 && (!count || (packets && request.ancillary)) && samples && samples % request.channels == 0,
            "invalid DeckLink audio/ANC inventory");
        require(!scheduled || (last_index != UINT64_MAX && index == last_index + 1), "DeckLink noncontiguous scheduled frame");
        if (pending.size() >= capacity) return 1;
        require(scheduled < uint64_t(INT64_MAX) / uint64_t(duration) && scheduled < uint64_t(INT64_MAX) / (48000 * uint64_t(duration)), "DeckLink timestamp overflow");
        require(index < uint64_t(INT64_MAX) / (48000 * uint64_t(duration)), "DeckLink source sample position overflow");
        const auto source_start = (index * 48000 * uint64_t(duration)) / uint64_t(scale);
        const auto source_end = ((index + 1) * 48000 * uint64_t(duration)) / uint64_t(scale);
        require(samples / request.channels == source_end - source_start, "DeckLink audio sample count does not match exact source frame cadence");
        const auto next_audio = audio_cursor + source_end - source_start;
        Pending item{}; item.index = index; item.audio.reserve(samples);
        for (uint32_t n = 0; n < samples; ++n) {
            require(audio[n] >= -8388608 && audio[n] <= 8388607, "DeckLink PCM24 sample out of range");
            item.audio.push_back(int32_t(int64_t(audio[n]) * 256));
        }
        std::map<uint32_t, std::vector<MdDeckLinkWirePacket>> by_line;
        for (uint32_t n = 0; n < count; ++n) {
            mondrian_decklink::validate_packet(packets[n], width);
            require(packets[n].line <= (height == 720 ? 25u : 41u), "ANC placement enters active picture");
            by_line[packets[n].line].push_back(packets[n]);
        }
        hr(output->CreateVideoFrame(int(width), int(height), int(row), bmdFormat10BitYUV, bmdFrameFlagDefault, item.frame.put()), "Create v210 output frame");
        void* pixels = nullptr; hr(item.frame->GetBytes(&pixels), "Get output frame storage"); require(pixels, "null output pixels"); std::memcpy(pixels, video, bytes);
        if (count) {
            Com<IDeckLinkVideoFrameAncillary> ancillary;
            hr(output->CreateAncillaryData(bmdFormat10BitYUV, ancillary.put()), "Create raw output VANC owner");
            for (const auto& entry : by_line) {
                void* buffer = nullptr; hr(ancillary->GetBufferForVerticalBlankingLine(entry.first, &buffer), "Get native output VANC line");
                mondrian_decklink::write_line(buffer, width, entry.second);
            }
            hr(item.frame->SetAncillaryData(ancillary.value), "Attach raw native VANC to scheduled frame");
        }
        // Retain the frame before SDK scheduling: completion callbacks can occur
        // synchronously. A partial audio/video admission poisons this owner.
        auto* key = static_cast<IDeckLinkVideoFrame*>(item.frame.value);
        pending.emplace(key, std::move(item)); auto& retained = pending.at(key);
        try {
            unsigned written = 0;
            hr(output->ScheduleAudioSamples(retained.audio.data(), samples / request.channels, BMDTimeValue(audio_cursor), 48000, &written), "Schedule exact SDI audio");
            require(written == samples / request.channels, "DeckLink partial audio scheduling invalidated session", -1);
            hr(output->ScheduleVideoFrame(key, BMDTimeValue(scheduled * uint64_t(duration)), duration, scale), "Schedule exact v210 SDI frame");
        } catch (const std::exception& error) { fault = error.what(); throw; }
        ++scheduled; last_index = index; audio_cursor = next_audio; return 0;
    }
    void pump() override {
        callback_fault();
        std::deque<Completion> ready;
        { std::lock_guard<std::mutex> guard(state->mutex); ready.swap(state->completed); }
        for (const auto& result : ready) {
            const auto found = pending.find(result.frame); require(found != pending.end(), "unknown/duplicate DeckLink completion", -1);
            if (result.result == bmdOutputFrameCompleted) {
                BMDTimeValue ticks = 0; hr(output->GetFrameCompletionReferenceTimestamp(result.frame, 48000, &ticks), "Read hardware frame-completion timestamp");
                require(ticks >= 0 && events.size() < capacity * 4 + 8, "invalid timestamp or event queue overflow", -1);
                events.push_back({1, 0, found->second.index, uint64_t(ticks)});
            } else if (!(stop_requested && result.result == bmdOutputFrameFlushed)) {
                pending.erase(found); throw Failure(-1, "DeckLink reported late, dropped or unexpected flushed output frame");
            }
            pending.erase(found);
        }
        if (!stop_requested) {
            const auto locked = reference(output.value);
            if (!reference_seen || locked != last_reference) {
                require(events.size() < capacity * 4 + 8, "reference event queue overflow", -1);
                events.push_back({locked == 1 ? 5u : 6u, 0, 0, 0}); last_reference = locked; reference_seen = true;
            }
            require(!request.require_reference || locked == 1, "DeckLink lost external reference lock", -1);
        }
    }
    void start() {
        healthy(); require(!running && scheduled >= request.preroll, "DeckLink preroll not filled");
        hr(output->EndAudioPreroll(), "End DeckLink audio preroll"); preroll = false;
        hr(output->StartScheduledPlayback(0, scale, 1.0), "Start scheduled DeckLink playback"); running = true;
    }
    void stop() override {
        stop_requested = true;
        if (running) { BMDTimeValue stopped_at = 0; hr(output->StopScheduledPlayback(0, &stopped_at, scale), "Stop DeckLink scheduled playback"); }
    }
    void close(MdDeckLinkShutdown& receipt) noexcept override {
        bool success = true; receipt.requested = 1; stop_requested = true;
        try { stop(); } catch (...) { success = false; }
        if (output && running) {
            bool inactive = false;
            for (uint32_t n = 0; n < 200; ++n) {
                BOOL active = TRUE;
                if (FAILED(output->IsScheduledPlaybackRunning(&active))) break;
                if (!active) { inactive = true; break; }
                std::this_thread::sleep_for(std::chrono::milliseconds(5));
            }
            success = inactive && success; running = !inactive;
        }
        receipt.stopped = !running;
        if (output && audio_enabled) {
            success = SUCCEEDED(output->FlushBufferedAudioSamples()) && success;
            audio_enabled = FAILED(output->DisableAudioOutput()); success = !audio_enabled && success;
        }
        if (output && video_enabled) { video_enabled = FAILED(output->DisableVideoOutput()); success = !video_enabled && success; }
        if (output && callback_set) {
            callbacks_closed = SUCCEEDED(output->SetScheduledFrameCompletionCallback(nullptr));
            success = callbacks_closed && success; callback_set = !callbacks_closed;
        }
        if (running || video_enabled || audio_enabled || !callbacks_closed) {
            receipt.outstanding_frames = pending.size(); quarantine(receipt); return;
        }
        // Disable may have delivered final flushed callbacks. They prove release,
        // not presentation. Every retained frame must be accounted for.
        try { if (configured) pump(); } catch (...) { success = false; }
        receipt.outstanding_frames = pending.size(); success = pending.empty() && success;
        pending.clear(); output.reset(); finish_common(receipt, success);
    }
};
struct Capture final : Owner {
    Com<IDeckLinkInput> input;
    std::deque<MdDeckLinkWireFrame> captured;
    uint32_t width = 0, height = 0;
    bool enabled = false, callback_set = false, running = false;
    Capture(uint64_t id, uint64_t gen, uint32_t video_mode, uint32_t bound) : Owner(id, gen, video_mode, bound) {}
    void initialize() override {
        common(); require(identity.input_mode_mask & (1u << mode), "DeckLink receiver does not support exact mode", -3);
        input = query<IDeckLinkInput>(device.value);
        Com<IDeckLinkDisplayMode> display; hr(input->GetDisplayMode(modes[mode], display.put()), "Get receiver mode");
        width = uint32_t(display->GetWidth()); height = uint32_t(display->GetHeight());
        require(display->GetFieldDominance() == bmdProgressiveFrame, "receiver must be progressive");
        (void)mondrian_decklink::row_bytes(width);
        config.integer(bmdDeckLinkConfigVideoInputConnection, bmdVideoConnectionSDI);
        config.integer(bmdDeckLinkConfigVideoInputConversionMode, bmdNoVideoInputConversion);
        config.boolean(bmdDeckLinkConfigCapture1080pAsPsF, false);
        hr(input->SetCallback(callback.value), "Set independent receiver callback"); callback_set = true; callbacks_closed = false;
        hr(input->EnableVideoInput(modes[mode], bmdFormat10BitYUV, bmdVideoInputFlagDefault), "Enable independent physical SDI receiver"); enabled = true; configured = true;
    }
    void pump() override {
        callback_fault();
        std::deque<Com<IDeckLinkVideoInputFrame>> frames;
        { std::lock_guard<std::mutex> guard(state->mutex); frames.swap(state->received); }
        for (const auto& frame : frames) {
            require(!(frame->GetFlags() & bmdFrameHasNoInputSource), "DeckLink receiver has no physical SDI source", -3);
            require(frame->GetWidth() == long(width) && frame->GetHeight() == long(height)
                && frame->GetPixelFormat() == bmdFormat10BitYUV && uint32_t(frame->GetRowBytes()) == mondrian_decklink::row_bytes(width), "receiver raster changed", -1);
            Com<IDeckLinkVideoFrameAncillary> ancillary;
            hr(frame->GetAncillaryData(ancillary.put()), "Get independent captured raw VANC");
            require(ancillary && ancillary->GetPixelFormat() == bmdFormat10BitYUV && ancillary->GetDisplayMode() == modes[mode], "raw captured VANC mode mismatch", -1);
            BMDTimeValue ticks = 0, duration = 0;
            hr(frame->GetHardwareReferenceTimestamp(48000, &ticks, &duration), "Get independent receiver hardware timestamp");
            require(ticks >= 0 && duration > 0, "invalid receiver hardware timestamp", -1);
            MdDeckLinkWireFrame result{}; result.capture_ticks = uint64_t(ticks);
            uint32_t readable = 0;
            for (uint32_t line = 1; line <= (height == 720 ? 25u : 41u); ++line) {
                void* buffer = nullptr; const auto status = ancillary->GetBufferForVerticalBlankingLine(line, &buffer);
                if (status == E_INVALIDARG) continue;
                hr(status, "read independent receiver VANC line"); ++readable;
                for (const auto& packet : mondrian_decklink::read_line(buffer, width, line)) {
                    require(result.count < 64, "receiver ANC inventory exceeds frame bound", -1); result.packets[result.count++] = packet;
                }
            }
            require(readable > 0, "receiver exposes no raw VANC lines", -4);
            require(captured.size() < capacity, "independent receiver queue overflow", -1); captured.push_back(result);
        }
    }
    void start() { healthy(); require(!running, "receiver already started"); hr(input->StartStreams(), "Start independent SDI receiver"); running = true; }
    void stop() override { stop_requested = true; if (running) { hr(input->StopStreams(), "Stop physical SDI receiver"); running = false; } }
    void close(MdDeckLinkShutdown& receipt) noexcept override {
        bool success = true; receipt.requested = 1;
        try { stop(); } catch (...) { success = false; }
        receipt.stopped = !running;
        if (input && enabled) { success = SUCCEEDED(input->FlushStreams()) && success; enabled = FAILED(input->DisableVideoInput()); success = !enabled && success; }
        if (input && callback_set) {
            callbacks_closed = SUCCEEDED(input->SetCallback(nullptr));
            success = callbacks_closed && success; callback_set = !callbacks_closed;
        }
        if (running || enabled || !callbacks_closed) {
            { std::lock_guard<std::mutex> guard(state->mutex); receipt.outstanding_frames = state->received.size(); }
            quarantine(receipt); return;
        }
        input.reset(); captured.clear(); finish_common(receipt, success);
    }
};
template<class T> int32_t consume(T* owner, MdDeckLinkShutdown* receipt) noexcept {
    if (!owner || !receipt) return -5;
    *receipt = {};
    const auto result = boundary([&] {
        if (owner->worker.joinable()) {
            owner->invoke([&] { owner->close(*receipt); });
            if (owner->quarantined) return -1;
            { std::lock_guard<std::mutex> guard(owner->mutex); owner->exit = true; }
            owner->wake.notify_one(); owner->worker.join(); receipt->worker_joined = owner->callbacks_closed ? 1u : 0u;
        } else { owner->close(*receipt); receipt->worker_joined = 1; }
        delete owner;
        return receipt->stopped && receipt->released && !receipt->outstanding_frames && !receipt->outstanding_resources ? 0 : -1;
    }, receipt->error, sizeof(receipt->error));
    return result;
}
}

uint32_t md_decklink_abi_version() { return 1; }
int32_t md_decklink_sdk_version(char* result, uint32_t bytes) {
    return boundary([&] { require(result && bytes >= 16, "invalid SDK version buffer"); text(result, bytes, BLACKMAGIC_DECKLINK_API_VERSION_STRING); return 0; }, nullptr, 0);
}
int32_t md_decklink_discover(MdDeckLinkDevice* result, uint32_t capacity, uint32_t* count, char* error, uint32_t bytes) {
    return boundary([&] {
        require(result && count && capacity && capacity <= 64, "invalid DeckLink discovery buffer"); *count = 0;
        return on_mta([&] {
            const auto driver = driver_version(); auto iterator = create<IDeckLinkIterator>(CLSID_CDeckLinkIterator);
            for (;;) {
                Com<IDeckLink> device; const auto status = iterator->Next(device.put());
                if (status == S_FALSE) break; hr(status, "discover DeckLink endpoint");
                require(*count < capacity && device, "DeckLink inventory exceeds bound");
                result[(*count)++] = describe(device.value, driver);
            }
            return 0;
        });
    }, error, bytes);
}
int32_t md_decklink_reference(uint64_t serial, uint64_t generation, int32_t* locked, char* error, uint32_t bytes) {
    return boundary([&] { require(locked, "null DeckLink reference result"); return on_mta([&] {
        MdDeckLinkDevice identity{}; auto device = open_exact(serial, generation, identity);
        auto output = query<IDeckLinkOutput>(device.value); *locked = reference(output.value); return 0;
    }); }, error, bytes);
}
int32_t md_decklink_open(uint64_t serial, uint64_t generation, const MdDeckLinkRequest* request, void** result, char* error, uint32_t bytes) {
    if (result) *result = nullptr;
    return boundary([&] { require(request && result, "null DeckLink open input");
        auto* owner = new Output(serial, generation, *request); *result = owner; owner->launch(); return 0;
    }, error, bytes);
}
int32_t md_decklink_schedule_vanc(void* pointer, uint64_t index, const uint8_t* video, uint32_t bytes, uint32_t row,
    const int32_t* audio, uint32_t samples, const MdDeckLinkWirePacket* packets, uint32_t count, char* error, uint32_t error_bytes) {
    return boundary([&] { require(pointer, "null DeckLink output owner"); auto* owner = static_cast<Output*>(pointer);
        return owner->invoke([&] { return owner->schedule(index, video, bytes, row, audio, samples, packets, count); });
    }, error, error_bytes);
}
int32_t md_decklink_start(void* pointer, char* error, uint32_t bytes) {
    return boundary([&] { require(pointer, "null DeckLink output owner"); auto* owner = static_cast<Output*>(pointer);
        owner->invoke([&] { owner->start(); }); return 0; }, error, bytes);
}
int32_t md_decklink_poll(void* pointer, MdDeckLinkEvent* result, char* error, uint32_t bytes) {
    return boundary([&] { require(pointer && result, "null DeckLink output poll"); auto* owner = static_cast<Output*>(pointer);
        return owner->invoke([&] { require(owner->configured, "DeckLink output poll without configuration", -1);
            require(owner->fault.empty(), owner->fault.c_str(), -1); owner->pump();
            if (owner->events.empty()) return 0; *result = owner->events.front(); owner->events.pop_front(); return 1; });
    }, error, bytes);
}
int32_t md_decklink_request_stop(void* pointer) {
    return boundary([&] { require(pointer, "null DeckLink stop owner"); auto* owner = static_cast<Output*>(pointer); owner->signal_stop(); return 0; }, nullptr, 0);
}
int32_t md_decklink_shutdown(void* pointer, MdDeckLinkShutdown* result) { return consume(static_cast<Output*>(pointer), result); }
int32_t md_decklink_capture_preflight(uint64_t serial, uint64_t generation, uint32_t mode, uint32_t line, uint32_t offset, char* error, uint32_t bytes) {
    return boundary([&] { require(mode < mode_count && line > 0 && line <= (mode >= 8 ? 25u : 41u)
        && uint64_t(offset) + 39 <= (mode >= 8 ? 1280u : 1920u), "receiver marker outside supported progressive luma VANC");
        return on_mta([&] { MdDeckLinkDevice identity{}; auto device = open_exact(serial, generation, identity);
            require(identity.input_mode_mask & (1u << mode), "receiver exact SDI mode unavailable", -3); idle(device.value); return 0; });
    }, error, bytes);
}
int32_t md_decklink_capture_open(uint64_t serial, uint64_t generation, uint32_t mode, uint32_t capacity, void** result, char* error, uint32_t bytes) {
    if (result) *result = nullptr;
    return boundary([&] { require(result, "null DeckLink receiver owner result"); auto* owner = new Capture(serial, generation, mode, capacity);
        *result = owner; owner->launch(); return 0; }, error, bytes);
}
int32_t md_decklink_capture_start(void* pointer, char* error, uint32_t bytes) {
    return boundary([&] { require(pointer, "null DeckLink receiver owner"); auto* owner = static_cast<Capture*>(pointer);
        owner->invoke([&] { owner->start(); }); return 0; }, error, bytes);
}
int32_t md_decklink_capture_poll(void* pointer, MdDeckLinkWireFrame* result, char* error, uint32_t bytes) {
    return boundary([&] { require(pointer && result, "null DeckLink receiver poll"); auto* owner = static_cast<Capture*>(pointer);
        return owner->invoke([&] { require(owner->configured, "DeckLink receiver poll without configuration", -1);
            require(owner->fault.empty(), owner->fault.c_str(), -1); owner->pump();
            if (owner->captured.empty()) return 0; *result = owner->captured.front(); owner->captured.pop_front(); return 1; });
    }, error, bytes);
}
int32_t md_decklink_capture_shutdown(void* pointer, MdDeckLinkShutdown* result) { return consume(static_cast<Capture*>(pointer), result); }
