# Buildscope real-build fixture for dependency and compiler observations.
cmake_minimum_required(VERSION 3.20)
project(buildscope_fixture LANGUAGES CXX)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)
option(ENABLE_PCH "Exercise PCH recording" OFF)
option(ENABLE_UNITY "Exercise unity identity" OFF)
add_custom_command(OUTPUT "${CMAKE_CURRENT_BINARY_DIR}/generated/config.hpp"
 COMMAND "${CMAKE_COMMAND}" "-DINPUT=${CMAKE_CURRENT_SOURCE_DIR}/generator_input.txt"
 "-DOUTPUT=${CMAKE_CURRENT_BINARY_DIR}/generated/config.hpp" -P "${CMAKE_CURRENT_SOURCE_DIR}/generate.cmake"
 DEPENDS generator_input.txt generate.cmake VERBATIM)
add_executable(app main.cpp first.cpp second.cpp "${CMAKE_CURRENT_BINARY_DIR}/generated/config.hpp")
target_include_directories(app PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/generated")
if(ENABLE_PCH)
 target_precompile_headers(app PRIVATE shared.hpp)
endif()
if(ENABLE_UNITY)
 set_target_properties(app PROPERTIES UNITY_BUILD ON)
endif()
