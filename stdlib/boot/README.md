# Stdlib Boot

`boot/` is the bottom of the stdlib tower. It brings the expander, core forms,
loader, gates, roots, and session command surface online.

Keep this directory small and explicit:

- `expander.caap` and `forms.caap` run before ordinary modules exist.
- Loader support modules publish registry entries used by `bootstrap.caap`.
- Opt-in profiles such as native emit and optimization compose on top of the
  base bootstrap.

Do not add domain libraries here just because they are convenient during boot.
If a helper can live in `lib/`, `syntax/`, or `semantics/`, prefer that and load
it deliberately from `bootstrap.caap`.
