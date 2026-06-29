# Stdlib Diagnostics Library

`lib/diag/` owns reusable diagnostic values and conversions. It should provide
data shapes and formatting helpers, not decide when a subsystem reports an
error.

Subsystems should keep their local policy close to the code that detects the
problem, then normalize through these helpers when they need shared diagnostic
bags or stable error records.
