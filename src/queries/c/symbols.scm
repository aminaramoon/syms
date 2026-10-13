; symwhere symbol query for C.
; Captures `@symbol.<kind>` on the NAME node of each definition and `@scope`
; on the name of scope-bearing nodes so the indexer can build container paths.
; Definitions only — no references, calls, parameters or locals.
;
; Declarator nesting note (tree-sitter-c 0.23): for `int *foo(void) {}` the
; pointer wrapper sits ABOVE the function_declarator
; (pointer_declarator -> function_declarator -> identifier), and
; parenthesized_declarator/attributed_declarator carry no `declarator` field,
; so each wrapper form is enumerated explicitly below.

; --------------------------------------------------- functions & prototypes
(function_definition
  declarator: [
    (function_declarator
      declarator: (identifier) @symbol.function)
    ; int *foo(void) {}
    (pointer_declarator
      declarator: (function_declarator
        declarator: (identifier) @symbol.function))
    ; int **foo(void) {}
    (pointer_declarator
      declarator: (pointer_declarator
        declarator: (function_declarator
          declarator: (identifier) @symbol.function)))
    ; int __attribute__((x)) foo(void) {}
    (attributed_declarator
      (function_declarator
        declarator: (identifier) @symbol.function))
    ; int (*fpdef(void))(int) {} — function returning a function pointer
    (function_declarator
      declarator: (parenthesized_declarator
        (pointer_declarator
          declarator: (function_declarator
            declarator: (identifier) @symbol.function)))
      parameters: (parameter_list))
    ; int (**dblptr(void))(int) {} — pointer to function pointer
    (function_declarator
      declarator: (parenthesized_declarator
        (pointer_declarator
          declarator: (pointer_declarator
            declarator: (function_declarator
              declarator: (identifier) @symbol.function)))))
  ])

; Prototypes (headers, extern blocks): void proto(int);
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
    (attributed_declarator
      (function_declarator
        declarator: (identifier) @symbol.function))
    ; int (*fpdef(void))(int); (note: function-pointer *variables* like
    ; `void (*gfp)(int);` do NOT match — their inner declarator is not a
    ; function_declarator)
    (function_declarator
      declarator: (parenthesized_declarator
        (pointer_declarator
          declarator: (function_declarator
            declarator: (identifier) @symbol.function)))
      parameters: (parameter_list))
    ; int (**dblptr(void))(int);
    (function_declarator
      declarator: (parenthesized_declarator
        (pointer_declarator
          declarator: (pointer_declarator
            declarator: (function_declarator
              declarator: (identifier) @symbol.function)))))
  ])

; ------------------------------------------------------------------- macros
(preproc_def name: (identifier) @symbol.macro)

(preproc_function_def name: (identifier) @symbol.macro)

; ------------------------------------------------------- aggregates & scopes
(struct_specifier
  name: (type_identifier) @symbol.struct @scope)

(union_specifier
  name: (type_identifier) @symbol.union @scope)

(enum_specifier
  name: (type_identifier) @symbol.enum @scope)

; enum members: enum E { EA, EB };
(enumerator
  name: (identifier) @symbol.enum-member)

; -------------------------------------------------------------------- fields
; struct/union members: int y; int *p; int arr[3]; int **pp; int (*cb)(int);
; (function_declarator with a direct field_identifier — i.e. a method
; prototype — cannot occur in C and is deliberately not a `field` branch)
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
; typedef int MyInt;  typedef int *FooPtr;  typedef int cb(int);
; typedef int (*cb)(int);  (multiple declarators are matched per declarator)
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
