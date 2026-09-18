#include "kg/db/NameDecode.h"

#include <regex>

namespace kg::db
{

namespace
{

// Exactly the delimiters sourcetrail::serializeNameHierarchyToDatabaseString()
// uses (third_party/sourcetraildb/src/NameHierarchy.cpp):
//
//   serialized = nameDelimiter + META_DELIMITER
//   for each element:
//       (NAME_DELIMITER between elements, none before the first)
//       element.name + PARTS_DELIMITER + element.prefix + SIGNATURE_DELIMITER + element.postfix
//
// i.e. PARTS_DELIMITER separates an element's `name` from its `prefix`
// (despite the name), and SIGNATURE_DELIMITER separates `prefix` from
// `postfix`. Confirmed against real fixture.srctrldb serialized_name values,
// e.g. (Square class, node id 23):
//   "::\tmfixture\ts\tp\tnSquare\ts\tp"
// decodes to delimiter "::", elements [{"","fixture",""}, {"","Square",""}].
constexpr const char* kMetaDelimiter = "\tm";
constexpr const char* kNameDelimiter = "\tn";
constexpr const char* kPartsDelimiter = "\ts";
constexpr const char* kSignatureDelimiter = "\tp";

// Phase 3's NameBuilder.cpp disambiguatorFor(): " [" + basename(declFile) +
// "#" + hex8(fnv1a64(declFile)) + "]", appended to a TU-local entity's own
// last-element postfix. hex8() always produces exactly 8 lowercase hex
// digits; basename can be any filename character except '[' or ']' (paths
// never legitimately contain those). Anchored at the end of the string.
const std::regex& disambiguatorPattern()
{
    static const std::regex re(R"( \[[^\[\]]*#[0-9a-f]{8}\]$)");
    return re;
}

}  // namespace

DecodedName decodeSerializedName(const std::string& serialized)
{
    DecodedName out;

    const std::string::size_type metaPos = serialized.find(kMetaDelimiter);
    if (metaPos == std::string::npos)
    {
        // Malformed/unexpected input -- degrade gracefully rather than
        // throwing, since this is on the hot path of every text/search
        // output; treat the whole string as a single unnamed-delimiter
        // element rather than crash the query tool over one bad row.
        out.nameDelimiter.clear();
        out.elements.push_back(DecodedNameElement{"", serialized, ""});
        return out;
    }

    out.nameDelimiter = serialized.substr(0, metaPos);
    const std::string rest = serialized.substr(metaPos + std::char_traits<char>::length(kMetaDelimiter));

    // Split `rest` on NAME_DELIMITER into one chunk per element.
    std::vector<std::string> chunks;
    std::string::size_type pos = 0;
    while (true)
    {
        const std::string::size_type next = rest.find(kNameDelimiter, pos);
        if (next == std::string::npos)
        {
            chunks.push_back(rest.substr(pos));
            break;
        }
        chunks.push_back(rest.substr(pos, next - pos));
        pos = next + std::char_traits<char>::length(kNameDelimiter);
    }

    out.elements.reserve(chunks.size());
    for (const std::string& chunk : chunks)
    {
        DecodedNameElement el;

        const std::string::size_type partsPos = chunk.find(kPartsDelimiter);
        std::string afterName;
        if (partsPos == std::string::npos)
        {
            el.name = chunk;
        }
        else
        {
            el.name = chunk.substr(0, partsPos);
            afterName = chunk.substr(partsPos + std::char_traits<char>::length(kPartsDelimiter));
        }

        const std::string::size_type sigPos = afterName.find(kSignatureDelimiter);
        if (sigPos == std::string::npos)
        {
            el.prefix = afterName;
        }
        else
        {
            el.prefix = afterName.substr(0, sigPos);
            el.postfix = afterName.substr(sigPos + std::char_traits<char>::length(kSignatureDelimiter));
        }

        out.elements.push_back(std::move(el));
    }

    return out;
}

std::string DecodedName::namePath() const
{
    std::string result;
    for (std::size_t i = 0; i < elements.size(); ++i)
    {
        if (i != 0)
        {
            result += nameDelimiter;
        }
        result += elements[i].name;
    }
    return result;
}

std::string DecodedName::qualifiedName() const
{
    std::string result = namePath();
    if (!elements.empty())
    {
        result += elements.back().postfix;
    }
    return result;
}

std::string DecodedName::displayName() const
{
    const std::string qn = qualifiedName();
    return std::regex_replace(qn, disambiguatorPattern(), "");
}

std::string DecodedName::signature() const
{
    if (elements.empty())
    {
        return "";
    }
    const DecodedNameElement& last = elements.back();
    return last.prefix + " " + last.name + last.postfix;
}

}  // namespace kg::db
