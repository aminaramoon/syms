# FindLibClang.cmake
#
# Manual discovery of libclang. We deliberately do NOT use find_package(Clang)
# here: it pulls in the full libTooling/LLVM CMake package with hundreds of
# imported targets we don't need, and is much slower / more fragile to
# resolve than a targeted search for the one library + header tree we
# actually link against.
#
# Defines, on success:
#   LibClang_INCLUDE_DIR   - directory containing clang-c/Index.h
#   LibClang_LIBRARY        - path to libclang.so (or equivalent)
#   LibClang_RESOURCE_DIR   - clang resource directory matching this libclang,
#                             suitable for passing straight to -resource-dir
#                             (i.e. <prefix>/lib/clang/<major> -- NOTE: the
#                             parent of the include/ directory that actually
#                             holds builtin headers like stddef.h, matching
#                             what `clang -print-resource-dir` reports; see
#                             the "Resource dir discovery" comment below for
#                             why this distinction matters)
#   LibClang::LibClang       - IMPORTED target
#
# Honors (in priority order): environment variables set by the flox
# activation hook (LIBCLANG_INCLUDE_DIR / LIBCLANG_LIBRARY_DIR /
# SYMHOW_RESOURCE_DIR), then $ENV{FLOX_ENV}, then normal system search paths.

find_path(LibClang_INCLUDE_DIR
    NAMES clang-c/Index.h
    HINTS
        $ENV{LIBCLANG_INCLUDE_DIR}
        $ENV{FLOX_ENV}/include
    PATH_SUFFIXES "" include
)

find_library(LibClang_LIBRARY
    NAMES clang libclang
    HINTS
        $ENV{LIBCLANG_LIBRARY_DIR}
        $ENV{FLOX_ENV}/lib
)

# --- Resource dir discovery -------------------------------------------------
# The resource dir is what clang's own `-resource-dir` flag expects (i.e.
# exactly what `clang -print-resource-dir` reports): a directory whose
# include/ subdirectory holds clang's builtin headers (stddef.h, stdarg.h,
# ...) matching the libclang version we linked against. IMPORTANT: this is
# the *parent* of that include/ directory, NOT the include/ directory
# itself -- clang appends "/include" internally when it looks up its own
# headers, so passing "-resource-dir=<...>/include" (one level too deep)
# silently breaks header discovery for every TU that pulls in any system
# header (found the hard way in Phase 3: `-resource-dir` pointed one level
# too deep, so `<stddef.h>` couldn't be found from e.g. libstdc++'s
# <cwchar>, even though the file existed on disk one directory up).
#
# Prefer the value precomputed by the flox activation hook (SYMHOW_RESOURCE_DIR,
# also expected to be the parent dir); otherwise probe
# $FLOX_ENV/lib/clang/<major> and pick the highest numeric <major> whose
# include/ subdirectory actually contains stddef.h.

if(DEFINED ENV{SYMHOW_RESOURCE_DIR} AND EXISTS "$ENV{SYMHOW_RESOURCE_DIR}/include/stddef.h")
    set(_symhow_resource_dir "$ENV{SYMHOW_RESOURCE_DIR}")
else()
    set(_symhow_resource_dir "")
    set(_symhow_best_major -1)
    file(GLOB _symhow_rd_candidates "$ENV{FLOX_ENV}/lib/clang/*")
    foreach(_symhow_cand ${_symhow_rd_candidates})
        if(IS_DIRECTORY "${_symhow_cand}" AND EXISTS "${_symhow_cand}/include/stddef.h")
            get_filename_component(_symhow_cand_name "${_symhow_cand}" NAME)
            if(_symhow_cand_name MATCHES "^[0-9]+$" AND _symhow_cand_name GREATER _symhow_best_major)
                set(_symhow_best_major "${_symhow_cand_name}")
                set(_symhow_resource_dir "${_symhow_cand}")
            endif()
        endif()
    endforeach()
    unset(_symhow_best_major)
    unset(_symhow_rd_candidates)
endif()

set(LibClang_RESOURCE_DIR "${_symhow_resource_dir}" CACHE PATH "clang resource dir matching libclang (pass directly to -resource-dir; its include/ subdir contains stddef.h etc.)")
unset(_symhow_resource_dir)

# --- Version discovery -------------------------------------------------
# libclang's C API version macros (CINDEX_VERSION_MAJOR/MINOR) don't track
# the LLVM release version, so instead parse the version out of the shared
# library's real (fully-resolved) filename, e.g.
# libclang.so -> libclang.so.21.1 -> libclang.so.21.1.8 resolves to
# "21.1.8".
set(LibClang_VERSION "unknown")
if(LibClang_LIBRARY)
    get_filename_component(_symhow_libclang_realpath "${LibClang_LIBRARY}" REALPATH)
    if(_symhow_libclang_realpath MATCHES "libclang(-cpp)?\\.so\\.([0-9]+\\.[0-9]+\\.[0-9]+)")
        set(LibClang_VERSION "${CMAKE_MATCH_2}")
    elseif(_symhow_libclang_realpath MATCHES "libclang(-cpp)?\\.so\\.([0-9]+\\.[0-9]+)")
        set(LibClang_VERSION "${CMAKE_MATCH_2}")
    elseif(_symhow_libclang_realpath MATCHES "libclang(-cpp)?\\.so\\.([0-9]+)")
        set(LibClang_VERSION "${CMAKE_MATCH_2}")
    endif()
    unset(_symhow_libclang_realpath)
endif()
set(LibClang_VERSION "${LibClang_VERSION}" CACHE STRING "Detected libclang version (best-effort, parsed from shared library filename)")

include(FindPackageHandleStandardArgs)
find_package_handle_standard_args(LibClang
    REQUIRED_VARS LibClang_LIBRARY LibClang_INCLUDE_DIR LibClang_RESOURCE_DIR
    VERSION_VAR LibClang_VERSION
)

if(LibClang_FOUND AND NOT TARGET LibClang::LibClang)
    add_library(LibClang::LibClang UNKNOWN IMPORTED)
    set_target_properties(LibClang::LibClang PROPERTIES
        IMPORTED_LOCATION "${LibClang_LIBRARY}"
        INTERFACE_INCLUDE_DIRECTORIES "${LibClang_INCLUDE_DIR}"
    )
endif()

mark_as_advanced(LibClang_INCLUDE_DIR LibClang_LIBRARY LibClang_RESOURCE_DIR)
