; symwhere symbol query for Rust.
; Captures `@symbol.<kind>` on the NAME node of each definition and `@scope`
; on the name of scope-bearing nodes so the indexer can build container paths.
; Definitions only — no references, calls, parameters or locals.
; Duplicate-capture note: a `fn` inside an `impl`/`trait` matches both the
; generic `@symbol.function` pattern and the container-specific
; `@symbol.method` pattern (tree-sitter queries cannot express ancestor
; negation); consumers should prefer the more specific (method) capture when
; both land on the same node.

; ---------------------------------------------------------------- functions
; Free functions, including `fn` items nested in modules or blocks.
(function_item
  name: (identifier) @symbol.function)

; Signatures without a body: extern blocks and trait declarations.
(function_signature_item
  name: (identifier) @symbol.function)

; ------------------------------------------------------------------- methods
; fn inside an impl block (includes `fn new`, the usual constructor).
(impl_item
  body: (declaration_list
    (function_item
      name: (identifier) @symbol.method)))

; fn — declared or defaulted — inside a trait.
(trait_item
  body: (declaration_list
    (function_item
      name: (identifier) @symbol.method)))

(trait_item
  body: (declaration_list
    (function_signature_item
      name: (identifier) @symbol.method)))

; -------------------------------------------------------------------- types
(struct_item
  name: (type_identifier) @symbol.struct @scope)

(union_item
  name: (type_identifier) @symbol.union @scope)

(enum_item
  name: (type_identifier) @symbol.enum @scope)

; enum variants: enum E { V1, V2(u32), V3 { f: u32 } }
(enum_variant
  name: (identifier) @symbol.enum-member)

; struct-variant fields.
(enum_variant
  body: (field_declaration_list
    (field_declaration
      name: (field_identifier) @symbol.field)))

; type aliases and trait associated types.
(type_item
  name: (type_identifier) @symbol.type-alias)

(associated_type
  name: (type_identifier) @symbol.type-alias)

; --------------------------------------------------------------------- impls
; `impl Foo` / `impl Trait for Foo` — the implemented type is the symbol; the
; impl block itself is a scope for its methods. Generic impls capture the
; full type text (e.g. `Foo<T>`).
(impl_item
  type: (type_identifier) @symbol.impl @scope)

(impl_item
  type: (generic_type) @symbol.impl @scope)

(impl_item
  type: (scoped_type_identifier) @symbol.impl @scope)

; -------------------------------------------------------------------- traits
(trait_item
  name: (type_identifier) @symbol.trait @scope)

; ------------------------------------------------------------------- modules
(mod_item
  name: (identifier) @symbol.module @scope)

; ---------------------------------------------------------- consts & statics
(const_item
  name: (identifier) @symbol.constant)

(static_item
  name: (identifier) @symbol.static)

; -------------------------------------------------------------------- macros
(macro_definition
  name: (identifier) @symbol.macro)

; -------------------------------------------------------------------- fields
(struct_item
  body: (field_declaration_list
    (field_declaration
      name: (field_identifier) @symbol.field)))

(union_item
  body: (field_declaration_list
    (field_declaration
      name: (field_identifier) @symbol.field)))
