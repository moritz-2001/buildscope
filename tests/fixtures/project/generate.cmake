# Buildscope real-build fixture for dependency and compiler observations.
file(READ "${INPUT}" VALUE)
string(STRIP "${VALUE}" VALUE)
get_filename_component(DIRECTORY "${OUTPUT}" DIRECTORY)
file(MAKE_DIRECTORY "${DIRECTORY}")
file(WRITE "${OUTPUT}.tmp" "#pragma once\ninline constexpr int generated_value = ${VALUE};\n")
execute_process(COMMAND "${CMAKE_COMMAND}" -E copy_if_different "${OUTPUT}.tmp" "${OUTPUT}" COMMAND_ERROR_IS_FATAL ANY)
file(REMOVE "${OUTPUT}.tmp")
