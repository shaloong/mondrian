// Independent ABI oracle. Compile against unmodified official 3.8.1 headers.
// The receipt collector must verify the actual include tree, compiler and binary.
#include "pluginterfaces/vst/ivstmidimapping2.h"
#include "pluginterfaces/vst/ivsttransportcontrol.h"
#include "pluginterfaces/vst/ivstnoteonorchestralarticulationinfo.h"
#include <cstddef>
#include <cstdio>
#include <type_traits>

using namespace Steinberg;
using namespace Steinberg::Vst;

// Independently type-check every new/changed signature in official headers.
static_assert(std::is_same_v<decltype(&IMidiMapping2::getNumMidi2ControllerAssignments), uint32 (PLUGIN_API IMidiMapping2::*)(BusDirections)>);
static_assert(std::is_same_v<decltype(&IMidiMapping2::getMidi2ControllerAssignments), tresult (PLUGIN_API IMidiMapping2::*)(BusDirections, const Midi2ControllerParamIDAssignmentList&)>);
static_assert(std::is_same_v<decltype(&IMidiMapping2::getNumMidi1ControllerAssignments), uint32 (PLUGIN_API IMidiMapping2::*)(BusDirections)>);
static_assert(std::is_same_v<decltype(&IMidiMapping2::getMidi1ControllerAssignments), tresult (PLUGIN_API IMidiMapping2::*)(BusDirections, const Midi1ControllerParamIDAssignmentList&)>);
static_assert(std::is_same_v<decltype(&IMidiLearn2::onLiveMidi2ControllerInput), tresult (PLUGIN_API IMidiLearn2::*)(BusIndex, MidiChannel, Midi2Controller)>);
static_assert(std::is_same_v<decltype(&IMidiLearn2::onLiveMidi1ControllerInput), tresult (PLUGIN_API IMidiLearn2::*)(BusIndex, MidiChannel, CtrlNumber)>);
static_assert(std::is_same_v<decltype(&ITransportControl::requestAction), tresult (PLUGIN_API ITransportControl::*)(int32, TransportPosition*)>);
static_assert(std::is_same_v<decltype(&ITransportControl::isActionSupported), tresult (PLUGIN_API ITransportControl::*)(int32)>);
static_assert(std::is_same_v<decltype(&NoteOnOrchestralArticulation::IInfo::getVariationsInfo), tresult (PLUGIN_API NoteOnOrchestralArticulation::IInfo::*)(int32, int16, NoteOnOrchestralArticulation::ClassificationVariations&)>);

static void printUid(const char* name, const TUID& uid)
{
    std::printf("\"%s\":\"", name);
    for (auto byte : uid)
        std::printf("%02x", static_cast<unsigned char>(byte));
    std::printf("\"");
}

int main()
{
    std::printf("{\"schema\":1,\"sdk_commit\":\"3cdf9ca5d1f5b1b21e0a86832aa4abe55607bd96\",");
    std::printf("\"pluginterfaces_commit\":\"4f547e8e102b47de4a8b8aaf343c73b700786372\",");
    std::printf("\"sdk_version\":\"%s\",\"pointer_bytes\":%zu,\"com_compatible\":%d,", SDKVersionString, sizeof(void*), COM_COMPATIBLE);
    std::printf("\"layouts\":{");
    std::printf("\"Midi2Controller\":{\"size\":%zu,\"align\":%zu,\"byte1\":%zu,\"byte2\":%zu},",
        sizeof(Midi2Controller), alignof(Midi2Controller), offsetof(Midi2Controller, byte1), offsetof(Midi2Controller, byte2));
    std::printf("\"Midi2ControllerParamIDAssignment\":{\"size\":%zu,\"align\":%zu,\"pId\":%zu,\"busIndex\":%zu,\"channel\":%zu,\"controller\":%zu},",
        sizeof(Midi2ControllerParamIDAssignment), alignof(Midi2ControllerParamIDAssignment),
        offsetof(Midi2ControllerParamIDAssignment, pId), offsetof(Midi2ControllerParamIDAssignment, busIndex),
        offsetof(Midi2ControllerParamIDAssignment, channel), offsetof(Midi2ControllerParamIDAssignment, controller));
    std::printf("\"Midi2ControllerParamIDAssignmentList\":{\"size\":%zu,\"align\":%zu,\"count\":%zu,\"map\":%zu},",
        sizeof(Midi2ControllerParamIDAssignmentList), alignof(Midi2ControllerParamIDAssignmentList),
        offsetof(Midi2ControllerParamIDAssignmentList, count), offsetof(Midi2ControllerParamIDAssignmentList, map));
    std::printf("\"Midi1ControllerParamIDAssignment\":{\"size\":%zu,\"align\":%zu,\"pId\":%zu,\"busIndex\":%zu,\"channel\":%zu,\"controller\":%zu},",
        sizeof(Midi1ControllerParamIDAssignment), alignof(Midi1ControllerParamIDAssignment),
        offsetof(Midi1ControllerParamIDAssignment, pId), offsetof(Midi1ControllerParamIDAssignment, busIndex),
        offsetof(Midi1ControllerParamIDAssignment, channel), offsetof(Midi1ControllerParamIDAssignment, controller));
    std::printf("\"Midi1ControllerParamIDAssignmentList\":{\"size\":%zu,\"align\":%zu,\"count\":%zu,\"map\":%zu},",
        sizeof(Midi1ControllerParamIDAssignmentList), alignof(Midi1ControllerParamIDAssignmentList),
        offsetof(Midi1ControllerParamIDAssignmentList, count), offsetof(Midi1ControllerParamIDAssignmentList, map));
    std::printf("\"TransportPosition\":{\"size\":%zu,\"align\":%zu,\"type\":%zu,\"samples\":%zu,\"quarterNotes\":%zu},",
        sizeof(TransportPosition), alignof(TransportPosition), offsetof(TransportPosition, type),
        offsetof(TransportPosition, samples), offsetof(TransportPosition, quarterNotes));
    using NoteOnOrchestralArticulation::ClassificationVariations;
    using NoteOnOrchestralArticulation::Variations;
    std::printf("\"Variations\":{\"size\":%zu,\"align\":%zu,\"variation\":%zu,\"variation_count\":%d},",
        sizeof(Variations), alignof(Variations), offsetof(Variations, variation), Variations::kNumSubClasses);
    std::printf("\"ClassificationVariations\":{\"size\":%zu,\"align\":%zu,\"classification\":%zu,\"classification_count\":%d}},",
        sizeof(ClassificationVariations), alignof(ClassificationVariations), offsetof(ClassificationVariations, classification), ClassificationVariations::kNumClassifications);
    std::printf("\"interfaces\":{");
    std::printf("\"IMidiMapping2\":{\"size\":%zu,\"align\":%zu},", sizeof(IMidiMapping2), alignof(IMidiMapping2));
    std::printf("\"IMidiLearn2\":{\"size\":%zu,\"align\":%zu},", sizeof(IMidiLearn2), alignof(IMidiLearn2));
    std::printf("\"ITransportControl\":{\"size\":%zu,\"align\":%zu},", sizeof(ITransportControl), alignof(ITransportControl));
    std::printf("\"NoteOnOrchestralArticulation::IInfo\":{\"size\":%zu,\"align\":%zu}},\"iids\":{",
        sizeof(NoteOnOrchestralArticulation::IInfo), alignof(NoteOnOrchestralArticulation::IInfo));
    printUid("IMidiMapping2", IMidiMapping2_iid); std::printf(",");
    printUid("IMidiLearn2", IMidiLearn2_iid); std::printf(",");
    printUid("ITransportControl", ITransportControl_iid); std::printf(",");
    printUid("NoteOnOrchestralArticulation::IInfo", NoteOnOrchestralArticulation::IInfo_iid);
    std::printf("}}\n");
    return 0;
}
