"""A small Python source fixture used by the stateless-ops golden test.

Both hosts run the same structural pattern (`match_code`) and the same recursive
splitter against this file. Because the matcher and splitter live in the shared
Rust engine, the Python and TypeScript results must be identical.
"""


def foo(a, b):
    return a + b


def bar(x):
    intermediate = process(x)
    return intermediate


def baz(first, second, third):
    total = foo(first, second)
    return process(total)
