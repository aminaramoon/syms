//! Inverse of `sourcetrail::serializeNameHierarchyToDatabaseString()` — what
//! `node.serialized_name` actually contains on disk. Mirrors
//! `symhow::db::NameDecode` (re-implements the tiny, schema-stable inverse
//! directly against the delimiter constants, without depending on
//! third_party/sourcetraildb).

use regex::Regex;

const META_DELIMITER: &str = "\tm";
const NAME_DELIMITER: &str = "\tn";
const PARTS_DELIMITER: &str = "\ts";
const SIGNATURE_DELIMITER: &str = "\tp";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DecodedNameElement {
    pub prefix: String,
    pub name: String,
    pub postfix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DecodedName {
    pub name_delimiter: String,
    pub elements: Vec<DecodedNameElement>,
}

impl DecodedName {
    /// Pure "A::B::C" path: elements' `.name` fields only, joined by
    /// `name_delimiter`.
    pub fn name_path(&self) -> String {
        let mut result = String::new();
        for (i, el) in self.elements.iter().enumerate() {
            if i != 0 {
                result.push_str(&self.name_delimiter);
            }
            result.push_str(&el.name);
        }
        result
    }

    /// `name_path()` with the LAST element's postfix appended verbatim.
    pub fn qualified_name(&self) -> String {
        let mut result = self.name_path();
        if let Some(last) = self.elements.last() {
            result.push_str(&last.postfix);
        }
        result
    }

    /// `qualified_name()` with the trailing disambiguator suffix stripped.
    pub fn display_name(&self) -> String {
        let qn = self.qualified_name();
        disambiguator_pattern().replace(&qn, "").to_string()
    }

    /// Last element's `prefix + " " + name + postfix`.
    pub fn signature(&self) -> String {
        match self.elements.last() {
            Some(last) => format!("{} {}{}", last.prefix, last.name, last.postfix),
            None => String::new(),
        }
    }
}

fn disambiguator_pattern() -> &'static Regex {
    // ` [<no brackets>#<8 lowercase hex>]` at the very end of the string.
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r" \[[^\[\]]*#[0-9a-f]{8}\]$").unwrap())
}

pub fn decode_serialized_name(serialized: &str) -> DecodedName {
    let mut out = DecodedName::default();

    let Some(meta_pos) = serialized.find(META_DELIMITER) else {
        // Malformed/unexpected input — degrade gracefully (see the C++
        // implementation's rationale: this is on the hot path of every
        // text/search output).
        out.elements.push(DecodedNameElement {
            prefix: String::new(),
            name: serialized.to_string(),
            postfix: String::new(),
        });
        return out;
    };

    out.name_delimiter = serialized[..meta_pos].to_string();
    let rest = &serialized[meta_pos + META_DELIMITER.len()..];

    // Split `rest` on NAME_DELIMITER into one chunk per element.
    for chunk in rest.split(NAME_DELIMITER) {
        let mut el = DecodedNameElement::default();
        let (name, after_name) = match chunk.find(PARTS_DELIMITER) {
            Some(parts_pos) => (
                &chunk[..parts_pos],
                &chunk[parts_pos + PARTS_DELIMITER.len()..],
            ),
            None => (chunk, ""),
        };
        el.name = name.to_string();
        match after_name.find(SIGNATURE_DELIMITER) {
            Some(sig_pos) => {
                el.prefix = after_name[..sig_pos].to_string();
                el.postfix = after_name[sig_pos + SIGNATURE_DELIMITER.len()..].to_string();
            }
            None => {
                el.prefix = after_name.to_string();
            }
        }
        out.elements.push(el);
    }

    out
}