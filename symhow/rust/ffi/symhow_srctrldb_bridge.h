// C ABI shim over the vendored C++ SourcetrailDB writer
// (third_party/sourcetraildb). This is the ONLY C++ code symhow's Rust
// backend links against: the Rust index layer drives the .srctrldb write
// path through this flat, opaque-handle interface, while every read and every
// symhow-private side table is pure Rust (rusqlite).
//
// All strings passed in are `const char*` (NUL-terminated UTF-8); all strings
// returned are either borrowed (valid until the next writer call) or
// heap-allocated with `symhow_free_string` for the caller to release.
#ifndef SYMHOW_SRCTRLDB_BRIDGE_H
#define SYMHOW_SRCTRLDB_BRIDGE_H

#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SymhowWriter SymhowWriter;

// One name-hierarchy element (mirrors sourcetrail::NameElement).
typedef struct
{
	const char* prefix;
	const char* name;
	const char* postfix;
} SymhowNameElement;

// 1-based line/column source range (mirrors sourcetrail::SourceRange).
typedef struct
{
	int fileId;
	int startLine;
	int startColumn;
	int endLine;
	int endColumn;
} SymhowSourceRange;

// ReferenceKind ordinals (must match third_party/sourcetraildb/include/ReferenceKind.h).
enum SymhowReferenceKind
{
	SYMHOW_REF_TYPE_USAGE = 0,
	SYMHOW_REF_USAGE = 1,
	SYMHOW_REF_CALL = 2,
	SYMHOW_REF_INHERITANCE = 3,
	SYMHOW_REF_OVERRIDE = 4,
	SYMHOW_REF_TYPE_ARGUMENT = 5,
	SYMHOW_REF_TEMPLATE_SPECIALIZATION = 6,
	SYMHOW_REF_INCLUDE = 7,
	SYMHOW_REF_IMPORT = 8,
	SYMHOW_REF_MACRO_USAGE = 9,
	SYMHOW_REF_ANNOTATION_USAGE = 10
};

SymhowWriter* symhow_writer_create(void);
void symhow_writer_destroy(SymhowWriter* w);

// Borrowed last-error string; valid until the next writer call.
const char* symhow_writer_last_error(SymhowWriter* w);

bool symhow_writer_open(SymhowWriter* w, const char* path);
bool symhow_writer_close(SymhowWriter* w);
bool symhow_writer_begin_transaction(SymhowWriter* w);
bool symhow_writer_commit_transaction(SymhowWriter* w);
bool symhow_writer_rollback_transaction(SymhowWriter* w);

// Returns the symbol id, or 0 on failure.
int symhow_writer_record_symbol(SymhowWriter* w, const char* nameDelimiter, const SymhowNameElement* elements, int count);
int symhow_writer_record_file(SymhowWriter* w, const char* path);
bool symhow_writer_record_file_language(SymhowWriter* w, int fileId, const char* language);
int symhow_writer_record_local_symbol(SymhowWriter* w, const char* name);
int symhow_writer_record_reference(SymhowWriter* w, int contextSymbolId, int referencedSymbolId, int referenceKind);
bool symhow_writer_record_symbol_location(SymhowWriter* w, int symbolId, SymhowSourceRange location);
bool symhow_writer_record_symbol_scope_location(SymhowWriter* w, int symbolId, SymhowSourceRange location);
bool symhow_writer_record_symbol_signature_location(SymhowWriter* w, int symbolId, SymhowSourceRange location);
bool symhow_writer_record_qualifier_location(SymhowWriter* w, int referencedSymbolId, SymhowSourceRange location);
bool symhow_writer_record_local_symbol_location(SymhowWriter* w, int localSymbolId, SymhowSourceRange location);
bool symhow_writer_record_reference_location(SymhowWriter* w, int referenceId, SymhowSourceRange location);
int symhow_writer_record_reference_to_unsolved(SymhowWriter* w, int contextSymbolId, int referenceKind, SymhowSourceRange location);
bool symhow_writer_record_error(SymhowWriter* w, const char* message, bool fatal, SymhowSourceRange location);

// Serializes a name hierarchy to the Sourcetrail database string form (the
// exact inverse of symhow::db::decodeSerializedName). Returns a heap string;
// the caller must release it with symhow_free_string.
char* symhow_serialize_name_hierarchy(const char* nameDelimiter, const SymhowNameElement* elements, int count);
void symhow_free_string(char* s);

#ifdef __cplusplus
}
#endif

#endif // SYMHOW_SRCTRLDB_BRIDGE_H