// Independent call-through oracle, compiled with unmodified official SDK headers.
// No Rust-generated header or handwritten replacement VST3 interface is included.
#include "pluginterfaces/vst/ivstmidimapping2.h"
#include "pluginterfaces/vst/ivsttransportcontrol.h"
#include "pluginterfaces/vst/ivstnoteonorchestralarticulationinfo.h"
#include <cstdint>

using namespace Steinberg;
using namespace Steinberg::Vst;

template <typename Interface> struct OwnedQuery
{
    Interface* pointer = nullptr;
    OwnedQuery() = default;
    OwnedQuery(const OwnedQuery&) = delete;
    OwnedQuery& operator=(const OwnedQuery&) = delete;
    ~OwnedQuery() { if (pointer) pointer->release(); }
    bool query(FUnknown* source, const TUID& iid)
    {
        void* result = nullptr;
        if (source->queryInterface(iid, &result) != kResultOk || result == nullptr)
            return false;
        pointer = static_cast<Interface*>(result);
        return true;
    }
};

static int exerciseInterfaces(FUnknown* unknown)
{
    OwnedQuery<IMidiMapping2> mapping;
    OwnedQuery<IMidiLearn2> learn;
    OwnedQuery<ITransportControl> transport;
    OwnedQuery<NoteOnOrchestralArticulation::IInfo> articulation;
    if (!mapping.query(unknown, IMidiMapping2_iid)) return 10;
    if (!learn.query(unknown, IMidiLearn2_iid)) return 11;
    if (!transport.query(unknown, ITransportControl_iid)) return 12;
    if (!articulation.query(unknown, NoteOnOrchestralArticulation::IInfo_iid)) return 13;
    if (mapping.pointer->getNumMidi2ControllerAssignments(kInput) != 2) return 20;
    Midi2ControllerParamIDAssignment midi2[2] {};
    const Midi2ControllerParamIDAssignmentList midi2List {2, midi2};
    if (mapping.pointer->getMidi2ControllerAssignments(kInput, midi2List) != kResultOk) return 21;
    if (midi2[0].pId != 0xF0FA0011u || midi2[0].busIndex != -3 || midi2[0].channel != 12 ||
        midi2[0].controller.byte1 != 0x93 || midi2[0].controller.byte2 != 0x22) return 22;
    if (midi2[1].pId != 0x80000001u || midi2[1].busIndex != 19 || midi2[1].channel != 15 ||
        midi2[1].controller.byte1 != 0xFF || midi2[1].controller.byte2 != 0x00) return 23;
    if (mapping.pointer->getNumMidi1ControllerAssignments(kOutput) != 1) return 24;
    Midi1ControllerParamIDAssignment midi1[1] {};
    const Midi1ControllerParamIDAssignmentList midi1List {1, midi1};
    if (mapping.pointer->getMidi1ControllerAssignments(kOutput, midi1List) != kResultOk) return 25;
    if (midi1[0].pId != 0xABCD1234u || midi1[0].busIndex != -1 || midi1[0].channel != 3 ||
        midi1[0].controller != 63) return 26;
    Midi2Controller controller {0xA6, 0xD3};
    if (learn.pointer->onLiveMidi2ControllerInput(-7, 13, controller) != kResultOk) return 30;
    if (learn.pointer->onLiveMidi1ControllerInput(11, 7, 64) != kResultOk) return 31;
    if (transport.pointer->isActionSupported(ITransportControl::Locate) != kResultOk) return 40;
    if (transport.pointer->isActionSupported(ITransportControl::RecordOnPlaybackStart) != kResultFalse) return 41;
    auto samples = TransportPosition::fromSamples(-1234567890123LL);
    if (transport.pointer->requestAction(ITransportControl::Locate, &samples) != kResultOk) return 42;
    auto quarters = TransportPosition::fromQuarterNotes(-123.25);
    if (transport.pointer->requestAction(ITransportControl::SetCycleEnd, &quarters) != kResultOk) return 43;
    if (transport.pointer->requestAction(ITransportControl::PlaybackStop, nullptr) != kResultOk) return 44;
    NoteOnOrchestralArticulation::ClassificationVariations variations {};
    if (articulation.pointer->getVariationsInfo(-9, 14, variations) != kResultOk) return 50;
    for (int classification = 0; classification < 17; ++classification)
        for (int variation = 0; variation < 16; ++variation)
            if (variations.classification[classification].variation[variation] !=
                static_cast<uint8>((classification * 17 + variation * 3) & 255)) return 51;
    // Query from nonzero interface offsets, then check canonical FUnknown identity.
    OwnedQuery<ITransportControl> crossQuery;
    if (!crossQuery.query(mapping.pointer, ITransportControl_iid)) return 60;
    if (crossQuery.pointer != transport.pointer) return 61;
    OwnedQuery<FUnknown> identity;
    if (!identity.query(articulation.pointer, FUnknown_iid)) return 62;
    if (identity.pointer != unknown) return 63;
    return 0;
}

#if defined(_WIN32)
#define MONDRIAN_TEST_EXPORT __declspec(dllexport)
#else
#define MONDRIAN_TEST_EXPORT __attribute__((visibility("default")))
#endif

extern "C" MONDRIAN_TEST_EXPORT int mondrian_sdk381_callthrough(FUnknown* unknown)
{
    if (unknown == nullptr) return 1;
    // Exactly two Rust owners are held by this fixture during the borrowed call.
    if (unknown->addRef() != 3) return 2;
    if (unknown->release() != 2) return 3;
    const int result = exerciseInterfaces(unknown);
    // OwnedQuery releases every acquired interface on all return paths.
    if (unknown->addRef() != 3) return 4;
    if (unknown->release() != 2) return 5;
    return result;
}
