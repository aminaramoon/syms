// A 5-level call chain, a self-recursive function, and two mutually
// recursive functions forming a genuine 2-cycle in the CALL graph.

int level4()
{
    return 4;
}

int level3()
{
    return level4() + 1;
}

int level2()
{
    return level3() + 1;
}

int level1()
{
    return level2() + 1;
}

int level0()
{
    return level1() + 1;
}

int factorial(int n)
{
    if (n <= 1)
    {
        return 1;
    }
    return n * factorial(n - 1);
}

int pong(int depth);

int ping(int depth)
{
    if (depth <= 0)
    {
        return 0;
    }
    return 1 + pong(depth - 1);
}

int pong(int depth)
{
    if (depth <= 0)
    {
        return 0;
    }
    return 1 + ping(depth - 1);
}

int runChain()
{
    return level0() + factorial(5) + ping(4);
}
