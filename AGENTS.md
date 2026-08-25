# Rust style guidelines

- Avoid defining functions inside of other function definitions.
- Prefer not to use helper functions unless they are both reusable and independently meaningful.
- Avoid creating helper functions that are only used in one place or which would be simpler to inline.
- Avoid iterator chains that combine operations such as mapping, filtering, and collecting. Prefer loops instead. Simple one-liners like `.into_iter().collect()` are ok.
- Avoid casts or `from`-conversions of `bool` to integer types. Use `if-else` instead.
- Function names should start with a verb.
- Include a doc comment for each function and type definition.
- Where useful, include additional comments, e.g. to clarify the meaning of struct fields and enum variants, and to explain non-obvious logic in functions.
