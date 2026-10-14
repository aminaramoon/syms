# SymhowWarnings.cmake
#
# Defines the INTERFACE target `symhow_options`, which every first-party symhow
# target (but NOT the vendored third_party/sourcetraildb target) should
# link against to pick up a consistent set of compiler warnings.
#
# Warnings are on by default but non-fatal. Set -DKG_STRICT=ON to promote
# them to errors (intended for CI).

add_library(symhow_options INTERFACE)

option(SYMHOW_STRICT "Treat warnings as errors" OFF)

target_compile_options(symhow_options INTERFACE
    -Wall
    -Wextra
    -Wpedantic
    -Wshadow
    -Wconversion
    -Wnon-virtual-dtor
)

if(SYMHOW_STRICT)
    target_compile_options(symhow_options INTERFACE -Werror)
endif()
