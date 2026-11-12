#ifndef SOURCETRAIL_VERSION_H
#define SOURCETRAIL_VERSION_H

// Hand-written replacement for upstream SourcetrailDB's core/version.h.in.
//
// Upstream generates this header via CMake configure_file() from
// core/version.h.in, substituting values parsed out of version.txt
// (upstream contents: "v4.db25.p1") using these regexes (see upstream
// CMakeLists.txt):
//   INTERFACE_VERSION = v(\d+)\..*        -> 4
//   DATABASE_VERSION  = v\d+\.db(\d+)\..* -> 25
//   PATCH_VERSION     = v\d+\.db\d+\.p(\d+).* -> 1
//   VERSION_STRING    = "v4.db25.p1"
//
// kg does not run that configure_file() step, so this file hard-codes the
// same macro names/values instead. See PROVENANCE.md for details.

#define INTERFACE_VERSION 4
#define DATABASE_VERSION 25
#define PATCH_VERSION 1

#define VERSION_STRING "v4.db25.p1"

#endif // SOURCETRAIL_VERSION_H
