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
