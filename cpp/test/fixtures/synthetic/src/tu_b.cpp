// See tu_a.cpp: deliberate USR-collision test case.

static void helper()
{
    int y = 2;
    y *= 2;
    (void)y;
}

int main()
{
    helper();
    return 0;
}
