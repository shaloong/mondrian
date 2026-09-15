# Bound native Makefile execution for Linux-hosted bundled dependencies.
#
# This file is inherited by nested CMake configure processes through the
# CMAKE_TOOLCHAIN_FILE environment variable. It intentionally does nothing for
# other hosts and generators.
if(CMAKE_HOST_SYSTEM_NAME STREQUAL "Linux" AND CMAKE_GENERATOR MATCHES "Unix Makefiles")
    set(
        CMAKE_MAKE_PROGRAM
        "${CMAKE_CURRENT_LIST_DIR}/linux-bounded-make.sh"
        CACHE FILEPATH
        "Mondrian bounded GNU Make launcher"
        FORCE
    )
endif()
