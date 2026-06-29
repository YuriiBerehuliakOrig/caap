//! IO host policy.
//!
//! The IO *operations* live in `caap_sys_runtime::io`; this module holds only
//! the caap-side stdin-read permission guard. See [`super::sys_policy`].

use std::cell::RefCell;
use std::rc::Rc;

use crate::values::{eval_err, EvalSignal};

use super::HostSystemPolicy;

pub(super) fn require_stdin_allowed(
    policy: &Rc<RefCell<HostSystemPolicy>>,
    context: &str,
) -> Result<(), EvalSignal> {
    if !policy.borrow().io.allow_stdin_read {
        return Err(eval_err(format!(
            "{context}: compile-time stdin reading is not allowed"
        )));
    }
    Ok(())
}
