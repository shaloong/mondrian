cmake_minimum_required(VERSION 3.24)
project(mondrian_reference_decklink LANGUAGES C CXX)
if(NOT WIN32 OR NOT MSVC OR NOT CMAKE_SIZEOF_VOID_P EQUAL 8)
  message(FATAL_ERROR "DeckLink COM bridge requires Windows x64 and Microsoft MIDL/MSVC")
endif()
set(CMAKE_MSVC_RUNTIME_LIBRARY MultiThreaded)
set(sdk "${CMAKE_CURRENT_SOURCE_DIR}/vendor/decklink-api-12.0")
set(generated "${CMAKE_CURRENT_BINARY_DIR}/generated")
file(MAKE_DIRECTORY "${generated}")
find_program(MONDRIAN_MIDL midl.exe HINTS
  "C:/Program Files (x86)/Windows Kits/10/bin/${CMAKE_VS_WINDOWS_TARGET_PLATFORM_VERSION}/x64"
  REQUIRED)
file(GLOB idl_dependencies CONFIGURE_DEPENDS "${sdk}/*.idl")
add_custom_command(OUTPUT "${generated}/DeckLinkAPI.h" "${generated}/DeckLinkAPI_i.c"
  COMMAND "${MONDRIAN_MIDL}" /nologo /env x64 /char signed /I "${sdk}"
    /out "${generated}" /h DeckLinkAPI.h /iid DeckLinkAPI_i.c /tlb DeckLinkAPI.tlb
    "${sdk}/DeckLinkAPI.idl"
  DEPENDS ${idl_dependencies} VERBATIM)
add_library(mondrian_reference_decklink SHARED bridge.cpp vanc_lines.cpp
  "${generated}/DeckLinkAPI_i.c" "${generated}/DeckLinkAPI.h")
target_include_directories(mondrian_reference_decklink PRIVATE "${generated}" "${sdk}")
target_compile_features(mondrian_reference_decklink PRIVATE cxx_std_17)
target_compile_definitions(mondrian_reference_decklink PRIVATE MD_DECKLINK_BUILD NOMINMAX WIN32_LEAN_AND_MEAN)
target_compile_options(mondrian_reference_decklink PRIVATE /W4 /WX /EHsc)
target_link_libraries(mondrian_reference_decklink PRIVATE ole32 oleaut32 uuid)
add_executable(mondrian_decklink_abi_probe abi_probe.cpp)
target_compile_features(mondrian_decklink_abi_probe PRIVATE cxx_std_17)
target_compile_options(mondrian_decklink_abi_probe PRIVATE /W4 /WX /EHsc)
target_link_libraries(mondrian_decklink_abi_probe PRIVATE mondrian_reference_decklink)
add_executable(mondrian_decklink_vanc_probe vanc_probe.cpp vanc_lines.cpp)
target_compile_features(mondrian_decklink_vanc_probe PRIVATE cxx_std_17)
target_compile_options(mondrian_decklink_vanc_probe PRIVATE /W4 /WX /EHsc)
enable_testing()
add_test(NAME decklink_abi_no_hardware COMMAND mondrian_decklink_abi_probe)
add_test(NAME decklink_vanc_boundaries COMMAND mondrian_decklink_vanc_probe)
add_executable(mondrian_decklink_lifecycle_probe lifecycle_probe.cpp vanc_lines.cpp
  "${generated}/DeckLinkAPI_i.c" "${generated}/DeckLinkAPI.h")
target_include_directories(mondrian_decklink_lifecycle_probe PRIVATE "${generated}" "${sdk}")
target_compile_features(mondrian_decklink_lifecycle_probe PRIVATE cxx_std_17)
target_compile_definitions(mondrian_decklink_lifecycle_probe PRIVATE MD_DECKLINK_BUILD NOMINMAX WIN32_LEAN_AND_MEAN)
target_compile_options(mondrian_decklink_lifecycle_probe PRIVATE /W4 /WX /EHsc)
target_link_libraries(mondrian_decklink_lifecycle_probe PRIVATE ole32 oleaut32 uuid)
add_test(NAME decklink_lifecycle_faults COMMAND mondrian_decklink_lifecycle_probe)
