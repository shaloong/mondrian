cmake_minimum_required(VERSION 3.21)
project(mondrian_ofx_readonly_ctor_controls LANGUAGES CXX)

set(MONDRIAN_WORKSPACE "" CACHE PATH "Explicit existing workspace root")
set(OFX_CTOR_PROPERTY_SOURCE "" CACHE FILEPATH "Exact frozen before or candidate PropertySuite cpp")
if(NOT IS_DIRECTORY "${MONDRIAN_WORKSPACE}/crates/mondrian-app/native/openfx/vendor")
  message(FATAL_ERROR "An explicit existing MONDRIAN_WORKSPACE is required")
endif()
if(NOT EXISTS "${OFX_CTOR_PROPERTY_SOURCE}")
  message(FATAL_ERROR "An explicit exact OFX_CTOR_PROPERTY_SOURCE is required")
endif()
set(OFX_VENDOR "${MONDRIAN_WORKSPACE}/crates/mondrian-app/native/openfx/vendor")
add_executable(readonly_suite_controls
  readonly_suite_controls.cpp
  "${OFX_CTOR_PROPERTY_SOURCE}")
target_compile_features(readonly_suite_controls PRIVATE cxx_std_17)
target_include_directories(readonly_suite_controls PRIVATE
  "${OFX_VENDOR}/include" "${OFX_VENDOR}/host/include")
target_compile_definitions(readonly_suite_controls PRIVATE NOMINMAX OFX_SUPPORTS_MULTITHREAD)
if(MSVC)
  target_compile_options(readonly_suite_controls PRIVATE /EHsc)
endif()
enable_testing()
add_test(NAME readonly_suite_controls COMMAND readonly_suite_controls)
