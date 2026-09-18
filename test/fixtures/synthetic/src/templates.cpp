// A class template and a function template, each with one call that's
// dependent on the template parameter (expected to become an UNSOLVED
// reference -- clang can't resolve it without instantiation) and one call
// that's non-dependent (expected to resolve normally). Both templates are
// instantiated at least once so there's concrete content to observe too.

void nonDependentHelper() {}

struct HasDependentCall
{
    void dependentCall() const {}
};

template <typename T>
struct Box
{
    T value{};

    void touch()
    {
        nonDependentHelper();  // non-dependent: resolves normally
        value.dependentCall();  // dependent on T: expected UNSOLVED
    }
};

template <typename T>
int genericFunc(T t)
{
    nonDependentHelper();  // non-dependent: resolves normally
    t.dependentCall();      // dependent on T: expected UNSOLVED
    return 0;
}

int instantiateTemplates()
{
    Box<HasDependentCall> box;
    box.touch();
    return genericFunc(HasDependentCall{});
}
