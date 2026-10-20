// C ABI shim implementation over sourcetrail::SourcetrailDBWriter.
#include "symhow_srctrldb_bridge.h"

#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

#include "NameHierarchy.h"
#include "ReferenceKind.h"
#include "SourceRange.h"
#include "SourcetrailDBWriter.h"

struct SymhowWriter
{
	sourcetrail::SourcetrailDBWriter writer;
};

static sourcetrail::NameHierarchy buildNameHierarchy(const char* nameDelimiter, const SymhowNameElement* elements, int count)
{
	sourcetrail::NameHierarchy nh;
	nh.nameDelimiter = nameDelimiter != nullptr ? nameDelimiter : "";
	nh.nameElements.reserve(static_cast<std::size_t>(count));
	for (int i = 0; i < count; ++i)
	{
		sourcetrail::NameElement el;
		el.prefix = elements[i].prefix != nullptr ? elements[i].prefix : "";
		el.name = elements[i].name != nullptr ? elements[i].name : "";
		el.postfix = elements[i].postfix != nullptr ? elements[i].postfix : "";
		nh.nameElements.push_back(std::move(el));
	}
	return nh;
}

static sourcetrail::SourceRange buildSourceRange(SymhowSourceRange r)
{
	sourcetrail::SourceRange sr;
	sr.fileId = r.fileId;
	sr.startLine = r.startLine;
	sr.startColumn = r.startColumn;
	sr.endLine = r.endLine;
	sr.endColumn = r.endColumn;
	return sr;
}

static sourcetrail::ReferenceKind toReferenceKind(int kind)
{
	switch (kind)
	{
		case SYMHOW_REF_TYPE_USAGE: return sourcetrail::ReferenceKind::TYPE_USAGE;
		case SYMHOW_REF_USAGE: return sourcetrail::ReferenceKind::USAGE;
		case SYMHOW_REF_CALL: return sourcetrail::ReferenceKind::CALL;
		case SYMHOW_REF_INHERITANCE: return sourcetrail::ReferenceKind::INHERITANCE;
		case SYMHOW_REF_OVERRIDE: return sourcetrail::ReferenceKind::OVERRIDE;
		case SYMHOW_REF_TYPE_ARGUMENT: return sourcetrail::ReferenceKind::TYPE_ARGUMENT;
		case SYMHOW_REF_TEMPLATE_SPECIALIZATION: return sourcetrail::ReferenceKind::TEMPLATE_SPECIALIZATION;
		case SYMHOW_REF_INCLUDE: return sourcetrail::ReferenceKind::INCLUDE;
		case SYMHOW_REF_IMPORT: return sourcetrail::ReferenceKind::IMPORT;
		case SYMHOW_REF_MACRO_USAGE: return sourcetrail::ReferenceKind::MACRO_USAGE;
		case SYMHOW_REF_ANNOTATION_USAGE: return sourcetrail::ReferenceKind::ANNOTATION_USAGE;
		default: return sourcetrail::ReferenceKind::USAGE;
	}
}

extern "C" {

SymhowWriter* symhow_writer_create(void)
{
	return new SymhowWriter();
}

void symhow_writer_destroy(SymhowWriter* w)
{
	delete w;
}

const char* symhow_writer_last_error(SymhowWriter* w)
{
	return w->writer.getLastError().c_str();
}

bool symhow_writer_open(SymhowWriter* w, const char* path)
{
	return w->writer.open(path != nullptr ? path : "");
}

bool symhow_writer_close(SymhowWriter* w)
{
	return w->writer.close();
}

bool symhow_writer_begin_transaction(SymhowWriter* w)
{
	return w->writer.beginTransaction();
}

bool symhow_writer_commit_transaction(SymhowWriter* w)
{
	return w->writer.commitTransaction();
}

bool symhow_writer_rollback_transaction(SymhowWriter* w)
{
	return w->writer.rollbackTransaction();
}

int symhow_writer_record_symbol(SymhowWriter* w, const char* nameDelimiter, const SymhowNameElement* elements, int count)
{
	return w->writer.recordSymbol(buildNameHierarchy(nameDelimiter, elements, count));
}

int symhow_writer_record_file(SymhowWriter* w, const char* path)
{
	return w->writer.recordFile(path != nullptr ? path : "");
}

bool symhow_writer_record_file_language(SymhowWriter* w, int fileId, const char* language)
{
	return w->writer.recordFileLanguage(fileId, language != nullptr ? language : "");
}

int symhow_writer_record_local_symbol(SymhowWriter* w, const char* name)
{
	return w->writer.recordLocalSymbol(name != nullptr ? name : "");
}

int symhow_writer_record_reference(SymhowWriter* w, int contextSymbolId, int referencedSymbolId, int referenceKind)
{
	return w->writer.recordReference(contextSymbolId, referencedSymbolId, toReferenceKind(referenceKind));
}

bool symhow_writer_record_symbol_location(SymhowWriter* w, int symbolId, SymhowSourceRange location)
{
	return w->writer.recordSymbolLocation(symbolId, buildSourceRange(location));
}

bool symhow_writer_record_symbol_scope_location(SymhowWriter* w, int symbolId, SymhowSourceRange location)
{
	return w->writer.recordSymbolScopeLocation(symbolId, buildSourceRange(location));
}

bool symhow_writer_record_symbol_signature_location(SymhowWriter* w, int symbolId, SymhowSourceRange location)
{
	return w->writer.recordSymbolSignatureLocation(symbolId, buildSourceRange(location));
}

bool symhow_writer_record_qualifier_location(SymhowWriter* w, int referencedSymbolId, SymhowSourceRange location)
{
	return w->writer.recordQualifierLocation(referencedSymbolId, buildSourceRange(location));
}

bool symhow_writer_record_local_symbol_location(SymhowWriter* w, int localSymbolId, SymhowSourceRange location)
{
	return w->writer.recordLocalSymbolLocation(localSymbolId, buildSourceRange(location));
}

bool symhow_writer_record_reference_location(SymhowWriter* w, int referenceId, SymhowSourceRange location)
{
	return w->writer.recordReferenceLocation(referenceId, buildSourceRange(location));
}

int symhow_writer_record_reference_to_unsolved(SymhowWriter* w, int contextSymbolId, int referenceKind, SymhowSourceRange location)
{
	return w->writer.recordReferenceToUnsolvedSymhol(contextSymbolId, toReferenceKind(referenceKind), buildSourceRange(location));
}

bool symhow_writer_record_error(SymhowWriter* w, const char* message, bool fatal, SymhowSourceRange location)
{
	return w->writer.recordError(message != nullptr ? message : "", fatal, buildSourceRange(location));
}

char* symhow_serialize_name_hierarchy(const char* nameDelimiter, const SymhowNameElement* elements, int count)
{
	const std::string serialized = sourcetrail::serializeNameHierarchyToDatabaseString(buildNameHierarchy(nameDelimiter, elements, count));
	char* out = static_cast<char*>(std::malloc(serialized.size() + 1));
	if (out == nullptr)
	{
		return nullptr;
	}
	std::memcpy(out, serialized.c_str(), serialized.size() + 1);
	return out;
}

void symhow_free_string(char* s)
{
	std::free(s);
}

} // extern "C"