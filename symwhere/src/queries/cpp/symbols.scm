; symwhere symbol query for C++ (also used for CUDA sources).
; Captures `@symbol.<kind>` on the NAME node of each definition and `@scope`
; on the name of scope-bearing nodes so the indexer can build container paths.
; Definitions only — no references, calls, parameters or locals.
;
; Grammar notes (tree-sitter-cpp 0.23, probe-verified):
; - `template_declaration` is a transparent wrapper; patterns anchored on the
;   wrapped definition nodes catch templated entities. `template_function` /
;   `template_method` are *call* expression nodes and are never captured.
; - Declarator wrappers sit ABOVE the function_declarator
;   (`int *foo()` -> pointer_declarator(function_declarator(...)),
;    `T& foo()` -> reference_declarator(function_declarator(...)));
;   reference_declarator/attributed_declarator/parenthesized_declarator have
;   no `declarator` field, so wrapper forms are enumerated explicitly.
; - `qualified_identifier` has fields `scope` + `name`. The scope part is
;   `template_type` when it carries template arguments (`RingBuffer<T>`) and
;   `namespace_identifier` otherwise — a NON-templated class scope parses as
;   `namespace_identifier`, so `Cls::method` and `ns::fn` are structurally
;   identical. Out-of-class definitions therefore pair shape patterns with
;   text predicates:
;     * `#eq?` scope==name → constructor (`Foo::Foo`, `RingBuffer<T>::RingBuffer`),
;       which also keeps `ns::fn()` and `Foo::bar()` out of the constructor
;       captures;
;     * `#match? "^[A-Z_]"` on the scope leaf → member/static heuristic: class
;       names are conventionally CamelCase, namespaces lowercase, so
;       `Foo::bar()` becomes a method while `ns::fn()` stays a free function.
;       An uppercase namespace (`Util::helper()`) is misread as a member — the
;       accepted tradeoff, since there is no structural difference to key on.
;   Predicates must be GROUPED with their pattern (`((pattern) (#pred ...))`):
;   a bare top-level predicate line compiles as its own empty no-op pattern
;   and silently filters nothing (probe-verified against the tree-sitter crate).
; - In-class member *declarations* are `field_declaration` nodes; ctor
;   declarations are `declaration` nodes with the class name as identifier.
; Container convention: containers come only from `@scope` spans that enclose
; the name node (index.rs `container_for`), and a leaf `@scope` expands to its
; parent span. For out-of-class definitions the indexer special-cases
; `qualified_identifier`: scope segments captured inside the QI's `scope`
; field are emitted as container segments (see index.rs `scope_entry` /
; `container_for`), so out-of-class members resolve to e.g. `dsp::RingBuffer`
; while the `#match?` gate keeps `ns::fn()` free of a bogus `ns` segment.
; Duplicate-capture note: constructors match both `@symbol.function`/`@symbol.method`
; and `@symbol.constructor`; consumers prefer the most specific kind captured
; on the same node (index.rs `choose_kind`).

; --------------------------------------------------------- free functions
; void foo() {}
; (wrapper alternation covers reference/pointer/attributed return types)
(function_definition
  declarator: [
    (function_declarator
      declarator: (identifier) @symbol.function)
    (pointer_declarator
      declarator: (function_declarator
        declarator: (identifier) @symbol.function))
    (pointer_declarator
      declarator: (pointer_declarator
        declarator: (function_declarator
          declarator: (identifier) @symbol.function)))
    (reference_declarator
      (function_declarator
        declarator: (identifier) @symbol.function))
    (reference_declarator
      (pointer_declarator
        declarator: (function_declarator
          declarator: (identifier) @symbol.function)))
    (attributed_declarator
      (function_declarator
        declarator: (identifier) @symbol.function))
  ])

; ------------------------------------------------------------------ methods
; Member functions defined inside a class body (field_identifier), including
; through reference/pointer return wrappers: void bar() {} / int& get() {}
(function_definition
  declarator: [
    (function_declarator
      declarator: (field_identifier) @symbol.method)
    (reference_declarator
      (function_declarator
        declarator: (field_identifier) @symbol.method))
    (pointer_declarator
      declarator: (function_declarator
        declarator: (field_identifier) @symbol.method))
  ])

; Member function prototypes declared in a class body: void bar();
(field_declaration
  declarator: [
    (function_declarator
      declarator: (field_identifier) @symbol.method)
    (reference_declarator
      (function_declarator
        declarator: (field_identifier) @symbol.method))
    (pointer_declarator
      declarator: (function_declarator
        declarator: (field_identifier) @symbol.method))
  ])

; Destructors, inline or declared: ~Foo() {} / ~Foo();
(function_definition
  declarator: (function_declarator
    declarator: (destructor_name) @symbol.method))

(declaration
  declarator: (function_declarator
    declarator: (destructor_name) @symbol.method))

; ---------------------------------------------------- out-of-class definitions
; Definitions outside the class body: `void Foo::bar() {}`,
; `RingBuffer<T>::push(...) {}`, `Foo::Foo(int x) {}`. Probe-verified CST:
;   - the scope part is `template_type` when it carries template arguments and
;     `namespace_identifier` otherwise (including for plain class scopes);
;   - the member name is an `identifier` leaf — never `field_identifier`, so
;     older field_identifier-based patterns here never fired and such defs
;     fell through to the generic `@symbol.function` pattern.
; See the header for the predicate strategy and the container caveat.

; Templated scopes are unambiguous members (namespaces cannot carry template
; arguments): T RingBuffer<T>::pop() {} / T& RingBuffer<T>::front() {} /
; int* Grid<T>::cell(int i) {}. Constructors also match the pattern below and
; win the dedupe.
(function_definition
  declarator: [
    (function_declarator
      declarator: (qualified_identifier
        scope: (template_type) @scope
        name: (identifier) @symbol.method))
    (reference_declarator
      (function_declarator
        declarator: (qualified_identifier
          scope: (template_type)
          name: (identifier) @symbol.method)))
    (pointer_declarator
      declarator: (function_declarator
        declarator: (qualified_identifier
          scope: (template_type)
          name: (identifier) @symbol.method)))
  ])

; Constructors of templated classes: RingBuffer<T>::RingBuffer(cap) {}.
; `#eq?` compares the class name inside template_type with the member name.
((function_definition
   declarator: (function_declarator
     declarator: (qualified_identifier
       scope: (template_type (type_identifier) @scope)
       name: (identifier) @symbol.constructor)))
 (#eq? @scope @symbol.constructor))

; Constructors, non-templated: Foo::Foo(int x) {} — the name==scope test keeps
; free functions (`ns::fn()`) and plain methods (`Foo::bar()`) out.
((function_definition
   declarator: (function_declarator
     declarator: (qualified_identifier
       scope: (namespace_identifier) @scope
       name: (identifier) @symbol.constructor)))
 (#eq? @scope @symbol.constructor))

; Non-templated member definitions via the uppercase-scope heuristic:
; void Foo::bar() {} / int& Foo::get() {} / int* Foo::ptr(). The `@scope`
; capture (leaf scope part, consumed by the indexer's qualified_identifier
; container logic — see the container note in the header).
((function_definition
   declarator: [
     (function_declarator
       declarator: (qualified_identifier
         scope: (namespace_identifier) @scope
         name: (identifier) @symbol.method))
     (reference_declarator
       (function_declarator
         declarator: (qualified_identifier
           scope: (namespace_identifier) @scope
           name: (identifier) @symbol.method)))
     (pointer_declarator
       declarator: (function_declarator
         declarator: (qualified_identifier
           scope: (namespace_identifier) @scope
           name: (identifier) @symbol.method)))])
 (#match? @scope "^[A-Z_]"))

; Destructors: Foo::~Foo() {} / RingBuffer<T>::~RingBuffer() {} — the
; destructor_name shape is member-specific enough on its own (no gate).
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      name: (destructor_name) @symbol.method)))

; Out-of-class conversion operators: Foo::operator bool() {} — here the
; qualified_identifier sits directly in the declarator slot (no
; function_declarator wrapper).
((function_definition
   declarator: (qualified_identifier
     scope: (namespace_identifier) @scope
     name: (operator_cast) @symbol.operator))
 (#match? @scope "^[A-Z_]"))

(function_definition
  declarator: (qualified_identifier
    scope: (template_type)
    name: (operator_cast) @symbol.operator))

; Out-of-line static data members: int Foo::count = 0; / int Foo::count; /
; template <typename T> int RingBuffer<T>::count = 0;. The uppercase heuristic
; separates members from namespace-scope variables (`int ns::g_var = 0;` — a
; qualified variable definition — stays uncaptured, as before).
((declaration
   declarator: [
     (init_declarator
       declarator: (qualified_identifier
         scope: (namespace_identifier) @scope
         name: (identifier) @symbol.static))
     (qualified_identifier
       scope: (namespace_identifier) @scope
       name: (identifier) @symbol.static)])
 (#match? @scope "^[A-Z_]"))

(declaration
  declarator: [
    (init_declarator
      declarator: (qualified_identifier
        scope: (template_type) @scope
        name: (identifier) @symbol.static))
    (qualified_identifier
      scope: (template_type) @scope
      name: (identifier) @symbol.static)
  ])

; ---------------------------------------------------------------- operators
; bool operator==(const T&); T& operator*(); operator bool(); — inline,
; declared, or out-of-class definitions.
(function_definition
  declarator: [
    (function_declarator
      declarator: (operator_name) @symbol.operator)
    (reference_declarator
      (function_declarator
        declarator: (operator_name) @symbol.operator))
    (pointer_declarator
      declarator: (function_declarator
        declarator: (operator_name) @symbol.operator))
  ])

(field_declaration
  declarator: [
    (function_declarator
      declarator: (operator_name) @symbol.operator)
    (reference_declarator
      (function_declarator
        declarator: (operator_name) @symbol.operator))
    (pointer_declarator
      declarator: (function_declarator
        declarator: (operator_name) @symbol.operator))
  ])

(function_definition
  declarator: [
    (function_declarator
      declarator: (qualified_identifier
        name: (operator_name) @symbol.operator))
    (reference_declarator
      (function_declarator
        declarator: (qualified_identifier
          name: (operator_name) @symbol.operator)))
  ])

; Conversion operators: operator bool() const {} / operator bool() const;
(function_definition
  declarator: (operator_cast) @symbol.operator)

(declaration
  declarator: (operator_cast) @symbol.operator)

; ------------------------------------------------------------- constructors
; Foo(int x) {} / Foo(int x); defined or declared inside its own class body.
; The generic @symbol.function pattern also matches these nodes; consumers
; should prefer the more specific `@symbol.constructor` capture.
((class_specifier
   name: (type_identifier) @class_name
   body: (field_declaration_list
     [
       (function_definition
         declarator: (function_declarator
           declarator: (identifier) @symbol.constructor))
       (declaration
         declarator: (function_declarator
           declarator: (identifier) @symbol.constructor))]))
 ; Properly grouped so the predicate binds to the pattern (a bare top-level
 ; `(#eq? ...)` line compiles as a separate no-op pattern). In-class
 ; type-less `declaration`s are constructors, so the #eq? is mostly
 ; belt-and-suspenders — it filters nested-class false positives where the
 ; outer class_specifier also matches.
 (#eq? @class_name @symbol.constructor))

; -------------------------------------------------------------- declarations
; Prototypes at namespace scope or in extern "C" blocks: void proto(int);
(declaration
  declarator: [
    (function_declarator
      declarator: (identifier) @symbol.function)
    (pointer_declarator
      declarator: (function_declarator
        declarator: (identifier) @symbol.function))
    (pointer_declarator
      declarator: (pointer_declarator
        declarator: (function_declarator
          declarator: (identifier) @symbol.function)))
    (reference_declarator
      (function_declarator
        declarator: (identifier) @symbol.function))
    (attributed_declarator
      (function_declarator
        declarator: (identifier) @symbol.function))
    (function_declarator
      declarator: (parenthesized_declarator
        (pointer_declarator
          declarator: (function_declarator
            declarator: (identifier) @symbol.function))))
  ])

; Qualified free-function definitions: void ns::fn() {} — lowercase namespace
; scopes fall through the member heuristic above, so these stay `function`;
; out-of-class member definitions additionally carry the more specific
; method/constructor captures, which win the dedupe.
(function_definition
  declarator: [
    (function_declarator
      declarator: (qualified_identifier
        name: (identifier) @symbol.function))
    (reference_declarator
      (function_declarator
        declarator: (qualified_identifier
          name: (identifier) @symbol.function)))
    (pointer_declarator
      declarator: (function_declarator
        declarator: (qualified_identifier
          name: (identifier) @symbol.function)))
  ])

; ------------------------------------------------------------------- macros
(preproc_def name: (identifier) @symbol.macro)

(preproc_function_def name: (identifier) @symbol.macro)

; ------------------------------------------------------ classes & scopes
(class_specifier
  name: (type_identifier) @symbol.class @scope)

(struct_specifier
  name: (type_identifier) @symbol.struct @scope)

(union_specifier
  name: (type_identifier) @symbol.union @scope)

(enum_specifier
  name: (type_identifier) @symbol.enum @scope)

; enum members: enum class Color { Red, Green };
(enumerator
  name: (identifier) @symbol.enum-member)

; --------------------------------------------------------------- namespaces
; namespace N {} / namespace a::b {}
(namespace_definition
  name: (namespace_identifier) @symbol.namespace @scope)

(namespace_definition
  name: (nested_namespace_specifier) @symbol.namespace @scope)

; -------------------------------------------------------------------- fields
; Data members: int field; int *p; int **pp; int arr[3]; int (*cb)();
; (function_declarator with a direct field_identifier is a method prototype
; and is captured as @symbol.method above, not as a field)
(field_declaration
  declarator: [
    (field_identifier) @symbol.field
    (pointer_declarator
      declarator: (field_identifier) @symbol.field)
    (pointer_declarator
      declarator: (pointer_declarator
        declarator: (field_identifier) @symbol.field))
    (array_declarator
      declarator: (field_identifier) @symbol.field)
    (array_declarator
      declarator: (pointer_declarator
        declarator: (field_identifier) @symbol.field))
    (pointer_declarator
      declarator: (array_declarator
        declarator: (field_identifier) @symbol.field))
    (attributed_declarator
      (field_identifier) @symbol.field)
    ; function-pointer member: int (*cb)(int);
    (function_declarator
      declarator: (parenthesized_declarator
        (pointer_declarator
          declarator: (field_identifier) @symbol.field)))
  ])

; ------------------------------------------------------------- type aliases
; typedef int MyInt; / using A = int;
(type_definition
  declarator: [
    (type_identifier) @symbol.type-alias
    (pointer_declarator
      declarator: (type_identifier) @symbol.type-alias)
    (pointer_declarator
      declarator: (pointer_declarator
        declarator: (type_identifier) @symbol.type-alias))
    (array_declarator
      declarator: (type_identifier) @symbol.type-alias)
    (attributed_declarator
      (type_identifier) @symbol.type-alias)
    (function_declarator
      declarator: (type_identifier) @symbol.type-alias)
    (function_declarator
      declarator: (parenthesized_declarator
        (pointer_declarator
          declarator: (type_identifier) @symbol.type-alias)))
  ])

(alias_declaration
  name: (type_identifier) @symbol.type-alias)

; ------------------------------------------------------------------ concepts
(concept_definition
  name: (identifier) @symbol.interface)
