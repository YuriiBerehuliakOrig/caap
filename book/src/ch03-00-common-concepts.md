# Common Programming Concepts

This chapter covers concepts that appear in almost every language and how they
work in CAAP: the shape of source code, binding names to values, the built-in
data types, defining functions, and controlling the flow of execution.

Everything in this chapter is part of the **kernel**, so every example runs
directly on the bare kernel:

```bash
$ caap example.caap
```

Two conventions before we start. CAAP is *homoiconic*: code is written as nested
lists, so a "statement" and an "expression" are the same thing — a *form* that
evaluates to a value. And CAAP is *expression-oriented*: there are no statements
that don't produce a value, and the value of a block of code is the value of its
last form. Keep both in mind and the rest follows naturally.
