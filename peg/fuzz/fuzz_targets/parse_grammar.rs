//! Fuzz target: compile arbitrary text as a grammar. `Grammar::try_new` must
//! reject nonsense with `Err`, never panic; when it accepts, parsing a small
//! input against the result must also stay panic-free.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|src: &str| {
    if let Ok(grammar) = caap_peg::Grammar::try_new(src) {
        let _ = caap_peg::parse("abc 123 (x+y)", &grammar);
    }
});
