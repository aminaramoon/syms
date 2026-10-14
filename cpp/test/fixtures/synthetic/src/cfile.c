/* Plain C translation unit, deliberately included in the synthetic fixture
 * so a real C TU (as opposed to every other fixture source, which is C++)
 * is exercised by `fixture_index` and the fixture-based unit tests -- see
 * the ArgSanitizer language-aware -std fallback fix (a .c file must never
 * get a C++ -std injected). */

struct point
{
    int x;
    int y;
};

int point_manhattan_distance(const struct point* a, const struct point* b)
{
    int dx = a->x - b->x;
    int dy = a->y - b->y;
    if (dx < 0)
    {
        dx = -dx;
    }
    if (dy < 0)
    {
        dy = -dy;
    }
    return dx + dy;
}
