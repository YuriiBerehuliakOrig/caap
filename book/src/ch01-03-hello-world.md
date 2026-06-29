## Hello, World!

Now that the tool builds, let's print something. We'll do it twice: first on the
bare kernel, then on the tower with real standard-out I/O — the contrast teaches
you the most important distinction in CAAP.

### On the Bare Kernel

Create a file named `hello.caap`:

```scheme
; hello.caap — runnable on the bare kernel
"hello, world"
```

Run it:

```bash
$ caap hello.caap
hello, world
```

The whole program is a single form: the string atom `"hello, world"`. On the
bare kernel, `caap file.caap` evaluates the file and **prints the final
value**. A string value prints as its text; an integer prints as a number;
`null` prints nothing. Successful bare evaluation exits `0`.

That semicolon starts a comment — everything from `;` to the end of the line is
trivia. (Block comments are written `#| … |#` or `/* … */`.)

Let's make it compute something:

```scheme
; greet.caap — runnable on the bare kernel
(string_concat_many "hello, " "world" "!")
```

```bash
$ caap greet.caap
hello, world!
```

`string_concat_many` is a kernel builtin that concatenates all of its string
arguments. The form `(string_concat_many "hello, " "world" "!")` is a call: head
`string_concat_many`, three string arguments.

### On the Tower, with `sys.io`

Printing *as a side effect* — writing to standard out mid-program rather than
returning a value — is a *host service*. Host services require **capabilities**,
and capabilities are granted by a **bootstrap**. This is the second run mode:

```bash
$ caap stdlib/bootstrap.caap PROGRAM
```

Here the compiler first runs `stdlib/bootstrap.caap` *with `sys` authority*,
bringing up the whole tower — including the `sys.io` module whose `println`
writes a value and a newline to stdout. A program that uses I/O is a *module*
that imports `sys.io`; you'll write your first one in Chapter 7 once you've met
modules, and you'll see how host services and capabilities fit together in
Chapter 14.

> **Why two ways to "print"?** It reflects CAAP's central distinction. The bare
> kernel is a pure evaluator with no ambient authority — it can compute a value
> but cannot touch the outside world. Side effects like writing to a terminal,
> reading a file, or opening a socket are *capabilities* that only the tower
> grants, and only when asked. Keep this split in mind; the whole effect system
> (Chapter 9) is built on it.

### Comments and Style

CAAP source uses `;` for line comments. By convention, a file often begins with
a comment naming the file and what it demonstrates, exactly like the examples in
the repository's `examples/` directory — which is
where many of this book's examples come from.

With "hello, world" behind us, the next section maps out the run modes precisely
so you always know which layer your code is talking to.
