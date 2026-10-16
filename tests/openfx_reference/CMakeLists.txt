cmake_minimum_required(VERSION 3.20)
project(mondrian_openfx_basic_reference LANGUAGES CXX)

set(OPENFX_SOURCE_DIR "" CACHE PATH "Official OpenFX checkout at the pinned revision")
if(NOT IS_DIRECTORY "${OPENFX_SOURCE_DIR}")
  message(FATAL_ERROR "Set OPENFX_SOURCE_DIR to an official OpenFX checkout")
endif()

find_package(Git REQUIRED)
execute_process(
  COMMAND "${GIT_EXECUTABLE}" -C "${OPENFX_SOURCE_DIR}" rev-parse HEAD
  RESULT_VARIABLE OPENFX_GIT_RESULT
  OUTPUT_VARIABLE OPENFX_COMMIT
  OUTPUT_STRIP_TRAILING_WHITESPACE)
if(NOT OPENFX_GIT_RESULT EQUAL 0 OR
   NOT OPENFX_COMMIT STREQUAL "e40728885390ec16276d11e00025de9b4282060c")
  message(FATAL_ERROR "The official OpenFX checkout must be at e40728885390ec16276d11e00025de9b4282060c")
endif()

if(APPLE)
  set(OPENFX_ARCH_DIR "MacOS")
elseif(WIN32)
  set(OPENFX_ARCH_DIR "Win64")
elseif(UNIX)
  set(OPENFX_ARCH_DIR "Linux-x86-64")
else()
  message(FATAL_ERROR "Unsupported OpenFX reference platform")
endif()

set(OPENFX_BUNDLE_DIR "${CMAKE_BINARY_DIR}/Basic.ofx.bundle" CACHE PATH
    "Output location of the external reference bundle")
set(OPENFX_BINARY_DIR "${OPENFX_BUNDLE_DIR}/Contents/${OPENFX_ARCH_DIR}")

add_library(Basic MODULE "${OPENFX_SOURCE_DIR}/Examples/Basic/basic.cpp")
target_compile_features(Basic PRIVATE cxx_std_17)
target_include_directories(Basic PRIVATE
  "${OPENFX_SOURCE_DIR}/include"
  "${OPENFX_SOURCE_DIR}/Examples/include")
set_target_properties(Basic PROPERTIES
  PREFIX ""
  SUFFIX ".ofx"
  LIBRARY_OUTPUT_DIRECTORY "${OPENFX_BINARY_DIR}"
  LIBRARY_OUTPUT_DIRECTORY_RELEASE "${OPENFX_BINARY_DIR}"
  RUNTIME_OUTPUT_DIRECTORY "${OPENFX_BINARY_DIR}"
  RUNTIME_OUTPUT_DIRECTORY_RELEASE "${OPENFX_BINARY_DIR}")

file(MAKE_DIRECTORY "${OPENFX_BUNDLE_DIR}/Contents")
configure_file("${OPENFX_SOURCE_DIR}/Examples/Basic/Info.plist"
  "${OPENFX_BUNDLE_DIR}/Contents/Info.plist" COPYONLY)

# Instrumented test fixture is separate from the untouched official reference.
set(OPENFX_PROBE_BUNDLE_DIR "${CMAKE_BINARY_DIR}/SessionProbe.ofx.bundle")
set(OPENFX_PROBE_BINARY_DIR "${OPENFX_PROBE_BUNDLE_DIR}/Contents/${OPENFX_ARCH_DIR}")
add_library(SessionProbe MODULE session_probe.cpp)
target_compile_features(SessionProbe PRIVATE cxx_std_17)
if(MSVC)
  target_compile_options(SessionProbe PRIVATE /utf-8)
endif()
target_include_directories(SessionProbe PRIVATE
  "${OPENFX_SOURCE_DIR}" "${OPENFX_SOURCE_DIR}/include"
  "${OPENFX_SOURCE_DIR}/Examples/include")
set_target_properties(SessionProbe PROPERTIES PREFIX "" SUFFIX ".ofx"
  LIBRARY_OUTPUT_DIRECTORY "${OPENFX_PROBE_BINARY_DIR}"
  LIBRARY_OUTPUT_DIRECTORY_RELEASE "${OPENFX_PROBE_BINARY_DIR}"
  RUNTIME_OUTPUT_DIRECTORY "${OPENFX_PROBE_BINARY_DIR}"
  RUNTIME_OUTPUT_DIRECTORY_RELEASE "${OPENFX_PROBE_BINARY_DIR}")
file(MAKE_DIRECTORY "${OPENFX_PROBE_BUNDLE_DIR}/Contents")
configure_file("${OPENFX_SOURCE_DIR}/Examples/Basic/Info.plist"
  "${OPENFX_PROBE_BUNDLE_DIR}/Contents/Info.plist" COPYONLY)

find_package(Threads REQUIRED)
target_link_libraries(SessionProbe PRIVATE Threads::Threads)

# Small native ownership control: actual tracked HostSupport and product Host,
# with test-only global allocator failure injection in the executable.
set(MONDRIAN_OPENFX_HOST "${CMAKE_CURRENT_LIST_DIR}/../../crates/mondrian-app/native/openfx")
add_executable(ImageMemoryOwnership EXCLUDE_FROM_ALL image_memory_ownership.cpp)
target_compile_features(ImageMemoryOwnership PRIVATE cxx_std_17)
target_compile_definitions(ImageMemoryOwnership PRIVATE NOMINMAX OFX_SUPPORTS_MULTITHREAD)
set_target_properties(ImageMemoryOwnership PROPERTIES INTERPROCEDURAL_OPTIMIZATION FALSE)
target_include_directories(ImageMemoryOwnership PRIVATE
  "${MONDRIAN_OPENFX_HOST}/vendor/include"
  "${MONDRIAN_OPENFX_HOST}/vendor/host/include"
  "${MONDRIAN_OPENFX_HOST}/host")
foreach(source ofxhBinary ofxhClip ofxhHost ofxhImageEffect ofxhImageEffectAPI
    ofxhInteract ofxhMemory ofxhParam ofxhPluginAPICache ofxhPluginCache
    ofxhPropertySuite ofxhUtilities)
  target_sources(ImageMemoryOwnership PRIVATE "${MONDRIAN_OPENFX_HOST}/vendor/host/src/${source}.cpp")
endforeach()
foreach(source image_clip effect_instance host_descriptor parameter_instance thread_suite aux_suite)
  target_sources(ImageMemoryOwnership PRIVATE "${MONDRIAN_OPENFX_HOST}/host/${source}.cpp")
endforeach()
if(MSVC)
  target_compile_options(ImageMemoryOwnership PRIVATE /EHsc /utf-8)
endif()
if(WIN32)
  target_link_libraries(ImageMemoryOwnership PRIVATE shell32)
endif()
target_link_libraries(ImageMemoryOwnership PRIVATE Threads::Threads ${CMAKE_DL_LIBS})
