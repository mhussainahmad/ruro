# Builds the example nodes into one plugin:
#   cmake -S examples/native -B build && cmake --build build
#   RURO_PLUGINS=$PWD/build/libexample_nodes.so ruro daemon
cmake_minimum_required(VERSION 3.16)
project(ruro_example_nodes C CXX)

set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# ruro.h and ruro.hpp: sdk/include in a checkout, /usr/include with the
# Debian package.
find_path(RURO_INCLUDE_DIR ruro/ruro.h HINTS ${CMAKE_CURRENT_LIST_DIR}/../../sdk/include)

add_library(example_nodes SHARED lowpass.c threshold.cpp)
target_include_directories(example_nodes PRIVATE ${RURO_INCLUDE_DIR})
# Only ruro_plugin_init needs to be visible.
set_target_properties(example_nodes PROPERTIES C_VISIBILITY_PRESET hidden CXX_VISIBILITY_PRESET hidden)
