# KgWarnings.cmake
#
# Defines the INTERFACE target `kg_options`, which every first-party kg
# target (but NOT the vendored third_party/sourcetraildb target) should
# link against to pick up a consistent set of compiler warnings.
#
# Warnings are on by default but non-fatal. Set -DKG_STRICT=ON to promote
# them to errors (intended for CI).

add_library(kg_options INTERFACE)

option(KG_STRICT "Treat warnings as errors" OFF)

target_compile_options(kg_options INTERFACE
    -Wall
    -Wextra
    -Wpedantic
    -Wshadow
    -Wconversion
    -Wnon-virtual-dtor
)

if(KG_STRICT)
    target_compile_options(kg_options INTERFACE -Werror)
endif()
