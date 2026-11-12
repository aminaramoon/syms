// Deliberate USR-collision test case (paired with tu_b.cpp): both files
// define their own `static void helper()` (same name, different bodies)
// and their own `int main()`. Because both `helper` (internal linkage) and
// `main` (special-cased by the SymbolKey disambiguation rule) are
// translation-unit-local, indexing both TUs must produce 4 distinct symbol
// nodes with 4 distinct qualified names -- not 2 merged ones.

static void helper()
{
    int x = 1;
    x += 1;
    (void)x;
}

int main()
{
    helper();
    return 0;
}
