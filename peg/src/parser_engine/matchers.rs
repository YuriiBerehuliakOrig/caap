//! Expression matchers for `PegEvaluator`, split out of `parser_engine` to keep
//! the evaluator file focused on entry points, dispatch, and state. These are
//! `PegEvaluator` methods in a separate file — behaviour is identical to having
//! them inline; only the physical grouping changed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use smallvec::SmallVec;

use crate::error::ParseError;
use crate::expr::PegExpr;
use crate::parser_compile::{ChoiceDispatch, MemoRuntimeConfig};
use crate::parser_engine_diag::{format_tok_label, match_newline_at, measure_indent};
use crate::parser_engine_state::{EvalCfg, ParseCtx, ParseOutcome, PegEvaluatorInit};
use crate::types::ParseValue;

use super::{fail_message, repeat_value, sequence_value, MatchedHook, PegEvaluator};

impl<'a> PegEvaluator<'a> {
    // ── Terminal matchers ──────────────────────────────────────────────────

    pub(super) fn match_literal(
        &mut self,
        pos: usize,
        literal: &str,
    ) -> Result<ParseOutcome, ParseError> {
        let pos = self.skip_trivia(pos)?;
        // The comparison examines at most `literal.len()` bytes from `pos`
        // (fewer on an early mismatch / short remainder) — record that span.
        self.note_read(pos, (pos + literal.len()).min(self.ctx.text.len()));
        if self.ctx.text[pos..].starts_with(literal) {
            let end = pos + literal.len();
            Ok(ParseOutcome::success(
                end,
                ParseValue::Text(Arc::from(literal)),
            ))
        } else {
            self.state
                .diag
                .record_expected_lazy(pos, || format!("literal '{literal}'"));
            Ok(ParseOutcome::failure(pos))
        }
    }

    pub(super) fn match_dot(&mut self, pos: usize) -> Result<ParseOutcome, ParseError> {
        let pos = self.skip_trivia(pos)?;
        if let Some(ch) = self.ctx.text[pos..].chars().next() {
            let end = pos + ch.len_utf8();
            self.note_read(pos, end);
            let mut s = String::with_capacity(ch.len_utf8());
            s.push(ch);
            Ok(ParseOutcome::success(end, ParseValue::Text(s.into())))
        } else {
            self.note_read(pos, pos);
            self.state.diag.record_expected(pos, "any character");
            Ok(ParseOutcome::failure(pos))
        }
    }

    pub(super) fn match_regex(
        &mut self,
        pos: usize,
        regex: &crate::expr::CompiledRegex,
    ) -> Result<ParseOutcome, ParseError> {
        let pos = self.skip_trivia(pos)?;
        // The examined extent (where the regex's automaton dies, anchored at
        // `pos`) is the same whether the match succeeds or fails — it is the
        // bytes the outcome depends on. Compute it only when tracking, falling
        // back to the whole remainder for oversized patterns.
        if self.cfg.track_read_extent {
            let examined = regex
                .examined_len(&self.ctx.text.as_bytes()[pos..])
                .map_or(self.ctx.text.len(), |len| pos + len);
            self.note_read(pos, examined);
        }
        let suffix = &self.ctx.text[pos..];
        match regex.inner.find(suffix) {
            // `inner` is anchored, so any match starts at 0; the guard is kept as
            // a defensive invariant.
            Some(m) if m.start() == 0 => {
                let end = pos + m.end();
                Ok(ParseOutcome::success(
                    end,
                    ParseValue::Text(Arc::from(&suffix[..m.end()])),
                ))
            }
            _ => {
                self.state
                    .diag
                    .record_expected_lazy(pos, || format!("/{}/", regex.pattern));
                Ok(ParseOutcome::failure(pos))
            }
        }
    }

    /// Grammar-level error recovery (`recover("sync", …)`). Skips input up to and
    /// including the earliest sync literal, or to end-of-input if none is found,
    /// always succeeding with a `<recovered>` node wrapping the skipped text.
    /// Fails only when already at end-of-input (nothing to recover) so it cannot
    /// loop zero-width inside a repetition.
    pub(super) fn match_recover(
        &mut self,
        pos: usize,
        syncs: &[String],
    ) -> Result<ParseOutcome, ParseError> {
        let text = self.ctx.text;
        if pos >= text.len() {
            self.note_read(pos, pos);
            self.state.diag.record_expected(pos, "recovery point");
            return Ok(ParseOutcome::failure(pos));
        }
        // Earliest occurrence of any sync literal at/after `pos`; consume through
        // its end. Absent any sync, recover the rest of the input.
        let rest = &text[pos..];
        let end = syncs
            .iter()
            .filter(|s| !s.is_empty())
            .filter_map(|s| rest.find(s.as_str()).map(|off| pos + off + s.len()))
            .min()
            .unwrap_or(text.len());
        // The scan examined everything up to the recovery end.
        self.note_read(pos, end);
        let skipped = ParseValue::Text(Arc::from(&text[pos..end]));
        Ok(ParseOutcome::success(
            end,
            ParseValue::Node(crate::expr::RECOVER_TAG.into(), Arc::new(vec![skipped])),
        ))
    }

    /// Match a single character against a native [`crate::expr::CharClass`].
    /// Examines exactly the one inspected character — an exact, regex-free read
    /// extent.
    pub(super) fn match_char_class(
        &mut self,
        pos: usize,
        class: &crate::expr::CharClass,
    ) -> Result<ParseOutcome, ParseError> {
        let pos = self.skip_trivia(pos)?;
        if let Some(ch) = self.ctx.text[pos..].chars().next() {
            let end = pos + ch.len_utf8();
            self.note_read(pos, end);
            if class.contains(ch) {
                return Ok(ParseOutcome::success(
                    end,
                    ParseValue::Text(Arc::from(&self.ctx.text[pos..end])),
                ));
            }
        }
        self.state
            .diag
            .record_expected_lazy(pos, || class.to_source());
        Ok(ParseOutcome::failure(pos))
    }

    // ── Predicate matchers ─────────────────────────────────────────────────

    pub(super) fn match_and(
        &mut self,
        pos: usize,
        child: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        // Lookahead: never retains host state changes nor input.
        let snapshot = self.spec_checkpoint();
        let outcome = self.parse_expr(child, pos, active)?;
        self.spec_rollback(snapshot);
        match outcome {
            ParseOutcome::Success { .. } => Ok(ParseOutcome::success(pos, ParseValue::Nil)),
            ParseOutcome::Failure { .. } => Ok(ParseOutcome::failure(pos)),
        }
    }

    pub(super) fn match_not(
        &mut self,
        pos: usize,
        child: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        // Negative lookahead: never retains host state changes nor input.
        let snapshot = self.spec_checkpoint();
        let outcome = self.parse_expr(child, pos, active)?;
        self.spec_rollback(snapshot);
        match outcome {
            ParseOutcome::Success { .. } => Ok(ParseOutcome::failure(pos)),
            ParseOutcome::Failure { .. } => Ok(ParseOutcome::success(pos, ParseValue::Nil)),
        }
    }

    pub(super) fn match_cut(&self, pos: usize) -> Result<ParseOutcome, ParseError> {
        Ok(ParseOutcome::success_with_cut(pos))
    }

    // ── Repetition matchers ────────────────────────────────────────────────

    pub(super) fn match_sequence(
        &mut self,
        pos: usize,
        items: &[PegExpr],
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let mut current_pos = pos;
        let mut values = Vec::new();
        let mut cut_seen = false;

        for item in items {
            match self.parse_expr(item, current_pos, active)? {
                ParseOutcome::Success {
                    pos: next,
                    value,
                    cut,
                } => {
                    current_pos = next;
                    if cut {
                        cut_seen = true;
                        continue;
                    }
                    values.push(value);
                }
                ParseOutcome::Failure { pos: next, cut: _ } => {
                    if cut_seen {
                        return Ok(ParseOutcome::failure_with_cut(next));
                    }
                    return Ok(ParseOutcome::failure(next));
                }
            }
        }

        Ok(sequence_value(current_pos, values))
    }

    pub(super) fn match_choice(
        &mut self,
        pos: usize,
        alternatives: &[PegExpr],
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let candidates: SmallVec<[usize; 8]> = (0..alternatives.len()).collect();
        self.run_choice(pos, alternatives, candidates, active)
    }

    /// Try `candidates` (0-based indices into `alternatives`) at `pos` in order:
    /// return the first success; on a committed (`cut`) failure stop immediately;
    /// otherwise fail at the furthest position any alternative reached. Shared by
    /// ordered and first-char-dispatched choice evaluation.
    ///
    /// When a [`crate::driver::ParseDriver`] is attached, this is also the seam
    /// for the Parse Effects Protocol: a `ChoiceEnter` effect can restrict/reorder
    /// the candidates, and after each syntactic success an `AltMatched` effect
    /// lets the host accept, reject (→ backtrack to the next candidate), commit,
    /// or fail. Host state is checkpointed per candidate and rolled back when a
    /// candidate fails or is rejected.
    fn run_choice(
        &mut self,
        pos: usize,
        alternatives: &[PegExpr],
        candidates: SmallVec<[usize; 8]>,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let candidates = if self.cfg.driver.is_some() {
            let rule = self.state.rule_stack.last().copied();
            match self.raise_effect(crate::driver::ParseEffect::ChoiceEnter {
                rule,
                alt_count: alternatives.len(),
                pos,
            }) {
                crate::driver::Directive::Fail(message) => {
                    return Err(
                        ParseError::new(message, pos, self.ctx.text.len()).with_code("driver_fail")
                    );
                }
                crate::driver::Directive::Restrict(order) => order
                    .into_iter()
                    .filter(|&idx| idx < alternatives.len())
                    .collect(),
                _ => candidates,
            }
        } else {
            candidates
        };

        let mut furthest = pos;
        for idx in candidates {
            let snapshot = self.spec_checkpoint();
            let result = self.parse_expr(&alternatives[idx], pos, active)?;
            match result {
                ParseOutcome::Success {
                    pos: end,
                    value,
                    cut,
                } => {
                    if self.cfg.driver.is_none() {
                        self.spec_commit(snapshot);
                        return Ok(ParseOutcome::Success {
                            pos: end,
                            value,
                            cut,
                        });
                    }
                    let rule = self.state.rule_stack.last().copied();
                    let directive = self.raise_effect(crate::driver::ParseEffect::AltMatched {
                        rule,
                        index: idx,
                        pos,
                        end,
                        value: &value,
                    });
                    match directive {
                        crate::driver::Directive::Reject => {
                            self.spec_rollback(snapshot);
                            self.state
                                .diag
                                .record_expected(end, "a semantically valid alternative");
                            if end > furthest {
                                furthest = end;
                            }
                            continue;
                        }
                        crate::driver::Directive::Fail(message) => {
                            self.spec_rollback(snapshot);
                            return Err(ParseError::new(message, pos, self.ctx.text.len())
                                .with_code("driver_fail"));
                        }
                        crate::driver::Directive::Accept(new_value) => {
                            self.spec_commit(snapshot);
                            return Ok(ParseOutcome::Success {
                                pos: end,
                                value: new_value,
                                cut,
                            });
                        }
                        crate::driver::Directive::Commit => {
                            self.spec_commit(snapshot);
                            return Ok(ParseOutcome::Success {
                                pos: end,
                                value,
                                cut: true,
                            });
                        }
                        crate::driver::Directive::Proceed
                        | crate::driver::Directive::Restrict(_) => {
                            self.spec_commit(snapshot);
                            return Ok(ParseOutcome::Success {
                                pos: end,
                                value,
                                cut,
                            });
                        }
                    }
                }
                ParseOutcome::Failure { pos: fp, cut: true } => {
                    self.spec_rollback(snapshot);
                    return Ok(ParseOutcome::failure_with_cut(fp));
                }
                ParseOutcome::Failure { pos: fp, .. } => {
                    self.spec_rollback(snapshot);
                    if fp > furthest {
                        furthest = fp;
                    }
                }
            }
        }
        Ok(ParseOutcome::failure(furthest))
    }

    /// Choice evaluation using a precomputed first-character dispatch table.
    ///
    /// Peeks at the current character (after trivia) and limits the candidates
    /// to the alternatives registered for that character, unioned with the
    /// defaults (alternatives that are nullable or have unknown first chars).
    /// Falls back to `match_choice` when no dispatch entry exists for the char.
    pub(super) fn match_choice_dispatched(
        &mut self,
        pos: usize,
        alternatives: &[PegExpr],
        dispatch: &ChoiceDispatch,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let (char_map, defaults) = dispatch;

        // Peek at the first character after trivia without consuming it.
        let peek_pos = self.skip_trivia(pos)?;
        let first_char = self.ctx.text[peek_pos..].chars().next();
        // The dispatch decision examined that lookahead character.
        if let Some(c) = first_char {
            self.note_read(peek_pos, peek_pos + c.len_utf8());
        }

        let mut indices: SmallVec<[usize; 8]> = SmallVec::new();
        match first_char {
            None => {
                // At EOF — only defaults apply.
                indices.extend(defaults.iter().copied());
            }
            Some(c) => match char_map.get(&c) {
                None => {
                    // No dispatch entry for this char → fall back to full linear scan.
                    return self.match_choice(pos, alternatives, active);
                }
                Some(char_indices) => {
                    // Union of char-specific and default indices in alternative order.
                    indices.extend(char_indices.iter().copied());
                    indices.extend(defaults.iter().copied());
                    indices.sort_unstable();
                    indices.dedup();
                }
            },
        }

        let candidates: SmallVec<[usize; 8]> = indices
            .iter()
            .filter(|&&idx| idx != 0 && idx <= alternatives.len())
            .map(|&idx| idx - 1)
            .collect();
        self.run_choice(pos, alternatives, candidates, active)
    }

    pub(super) fn match_optional(
        &mut self,
        pos: usize,
        child: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let snapshot = self.spec_checkpoint();
        match self.parse_expr(child, pos, active)? {
            ParseOutcome::Success {
                pos: end, value, ..
            } => {
                self.spec_commit(snapshot);
                Ok(ParseOutcome::success(end, value))
            }
            ParseOutcome::Failure { .. } => {
                self.spec_rollback(snapshot);
                Ok(ParseOutcome::success(pos, ParseValue::Nil))
            }
        }
    }

    pub(super) fn match_one_or_more(
        &mut self,
        pos: usize,
        child: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let first_snapshot = self.spec_checkpoint();
        let first = self.parse_expr(child, pos, active)?;
        let (mut cur, first_val) = match first {
            ParseOutcome::Failure { .. } => {
                self.spec_rollback(first_snapshot);
                return Ok(first);
            }
            ParseOutcome::Success { pos, value, .. } => {
                self.spec_commit(first_snapshot);
                (pos, value)
            }
        };
        let mut values = Vec::with_capacity(4);
        values.push(first_val);
        loop {
            let snapshot = self.spec_checkpoint();
            match self.parse_expr(child, cur, active)? {
                ParseOutcome::Success { pos, value, .. } if pos > cur => {
                    self.spec_commit(snapshot);
                    values.push(value);
                    cur = pos;
                }
                _ => {
                    self.spec_rollback(snapshot);
                    break;
                }
            }
        }
        Ok(repeat_value(cur, values, "one_or_more"))
    }

    pub(super) fn match_zero_or_more(
        &mut self,
        pos: usize,
        child: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let mut cur = pos;
        let mut values = Vec::with_capacity(4);
        loop {
            let snapshot = self.spec_checkpoint();
            match self.parse_expr(child, cur, active)? {
                ParseOutcome::Success { pos, value, .. } if pos > cur => {
                    self.spec_commit(snapshot);
                    values.push(value);
                    cur = pos;
                }
                _ => {
                    self.spec_rollback(snapshot);
                    break;
                }
            }
        }
        Ok(repeat_value(cur, values, "zero_or_more"))
    }

    /// Operator-precedence expression via precedence climbing (no left
    /// recursion). Produces left/right-nested `Node("binop", [lhs, op, rhs])`.
    pub(super) fn match_precedence(
        &mut self,
        pos: usize,
        operand: &PegExpr,
        levels: &[crate::expr::PrecLevel],
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        self.parse_precedence_bp(pos, operand, levels, 0, active)
    }

    /// Precedence climbing with a minimum binding power `min_bp`.
    fn parse_precedence_bp(
        &mut self,
        pos: usize,
        operand: &PegExpr,
        levels: &[crate::expr::PrecLevel],
        min_bp: usize,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        use crate::expr::Fixity;

        // ── nud: a prefix operator, else the bare operand ──────────────────
        let (mut lhs, mut cur) = if let Some((op_value, op_end, prec, _)) =
            self.peek_prec_op(pos, levels, &[Fixity::Prefix], active)?
        {
            // The prefix binds its operand at its own precedence.
            match self.parse_precedence_bp(op_end, operand, levels, prec, active)? {
                ParseOutcome::Success {
                    pos: end, value, ..
                } => (
                    ParseValue::Node(Arc::from("unary_prefix"), Arc::new(vec![op_value, value])),
                    end,
                ),
                _ => {
                    self.state
                        .diag
                        .record_expected(op_end, "operand after prefix operator");
                    return Ok(ParseOutcome::failure(op_end));
                }
            }
        } else {
            match self.parse_expr(operand, pos, active)? {
                ParseOutcome::Success {
                    pos: end, value, ..
                } => (value, end),
                failure => return Ok(failure),
            }
        };

        // ── led: infix, postfix and ternary operators ──────────────────────
        while let Some((op_value, op_end, prec, fixity)) = self.peek_prec_op(
            cur,
            levels,
            &[
                Fixity::InfixLeft,
                Fixity::InfixRight,
                Fixity::InfixNon,
                Fixity::Postfix,
                Fixity::Ternary,
            ],
            active,
        )? {
            if prec < min_bp {
                break;
            }
            match fixity {
                Fixity::Postfix => {
                    lhs =
                        ParseValue::Node(Arc::from("unary_postfix"), Arc::new(vec![lhs, op_value]));
                    cur = op_end;
                }
                Fixity::InfixLeft | Fixity::InfixRight | Fixity::InfixNon => {
                    // Left- and non-assoc bind the RHS one level tighter (so the
                    // RHS can't grab another same-level operator); right-assoc
                    // re-enters at the same level.
                    let next_min = if fixity == Fixity::InfixRight {
                        prec
                    } else {
                        prec + 1
                    };
                    match self.parse_precedence_bp(op_end, operand, levels, next_min, active)? {
                        ParseOutcome::Success {
                            pos: rhs_end,
                            value: rhs,
                            ..
                        } => {
                            lhs = ParseValue::Node(
                                Arc::from("binop"),
                                Arc::new(vec![lhs, op_value, rhs]),
                            );
                            cur = rhs_end;
                            // Non-associative: a second operator at the same
                            // precedence (`a == b == c`) is an error.
                            if fixity == Fixity::InfixNon {
                                if let Some((_, _, next_prec, _)) =
                                    self.peek_prec_op(cur, levels, &[Fixity::InfixNon], active)?
                                {
                                    if next_prec == prec {
                                        self.state.diag.record_expected(
                                            cur,
                                            "no chaining of a non-associative operator",
                                        );
                                        return Ok(ParseOutcome::failure(cur));
                                    }
                                }
                            }
                        }
                        _ => {
                            self.state
                                .diag
                                .record_expected(op_end, "operand after infix operator");
                            return Ok(ParseOutcome::failure(op_end));
                        }
                    }
                }
                Fixity::Ternary => {
                    // `cond ? then : else` — operators[0]=`?` (matched), [1]=`:`.
                    let close = &levels[prec].operators[1];
                    let (then_val, then_end) =
                        match self.parse_precedence_bp(op_end, operand, levels, 0, active)? {
                            ParseOutcome::Success {
                                pos: end, value, ..
                            } => (value, end),
                            _ => {
                                self.state
                                    .diag
                                    .record_expected(op_end, "expression after '?'");
                                return Ok(ParseOutcome::failure(op_end));
                            }
                        };
                    let close_end = match self.parse_expr(close, then_end, active)? {
                        ParseOutcome::Success { pos: end, .. } => end,
                        _ => {
                            self.state.diag.record_expected(then_end, "':' of ternary");
                            return Ok(ParseOutcome::failure(then_end));
                        }
                    };
                    // Right-associative else branch (re-enters at this level).
                    match self.parse_precedence_bp(close_end, operand, levels, prec, active)? {
                        ParseOutcome::Success {
                            pos: else_end,
                            value: else_val,
                            ..
                        } => {
                            lhs = ParseValue::Node(
                                Arc::from("ternary"),
                                Arc::new(vec![lhs, then_val, else_val]),
                            );
                            cur = else_end;
                        }
                        _ => {
                            self.state
                                .diag
                                .record_expected(close_end, "expression after ':'");
                            return Ok(ParseOutcome::failure(close_end));
                        }
                    }
                }
                Fixity::Prefix => unreachable!("prefix not requested in led"),
            }
        }
        Ok(ParseOutcome::success(cur, lhs))
    }

    /// Find the first operator at `pos` whose level fixity is in `want` (lowest
    /// level first, then declared order), returning value, end, and precedence.
    ///
    /// Pure lookahead: host state touched while testing operators is rolled back
    /// (an operator past a precedence boundary is not consumed). Operator
    /// matchers are expected to be side-effect-free.
    fn peek_prec_op(
        &mut self,
        pos: usize,
        levels: &[crate::expr::PrecLevel],
        want: &[crate::expr::Fixity],
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<Option<(ParseValue, usize, usize, crate::expr::Fixity)>, ParseError> {
        for (prec, level) in levels.iter().enumerate() {
            if !want.contains(&level.fixity) {
                continue;
            }
            // A ternary level is triggered only by its open marker (`?`); the
            // close marker (`:`) is consumed inside the ternary led arm.
            let ops: &[PegExpr] = if level.fixity == crate::expr::Fixity::Ternary {
                &level.operators[..1]
            } else {
                &level.operators
            };
            for op in ops {
                let snapshot = self.spec_checkpoint();
                let outcome = self.parse_expr(op, pos, active)?;
                self.spec_rollback(snapshot);
                if let ParseOutcome::Success {
                    pos: end, value, ..
                } = outcome
                {
                    return Ok(Some((value, end, prec, level.fixity)));
                }
            }
        }
        Ok(None)
    }

    /// Counted repetition `e{min,max}` — match `e` between `min` and `max`
    /// (inclusive; `None` = unbounded) times, failing if fewer than `min` match.
    pub(super) fn match_repeat(
        &mut self,
        pos: usize,
        child: &PegExpr,
        min: usize,
        max: Option<usize>,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let mut cur = pos;
        let mut values = Vec::new();
        loop {
            if max.is_some_and(|mx| values.len() >= mx) {
                break;
            }
            let snapshot = self.spec_checkpoint();
            match self.parse_expr(child, cur, active)? {
                ParseOutcome::Success {
                    pos: next, value, ..
                } if next > cur => {
                    self.spec_commit(snapshot);
                    values.push(value);
                    cur = next;
                }
                // Non-advancing match or failure: stop (a zero-width match can
                // never reach `min` and would loop forever).
                _ => {
                    self.spec_rollback(snapshot);
                    break;
                }
            }
        }
        if values.len() < min {
            let got = values.len();
            self.state
                .diag
                .record_expected_lazy(cur, || format!("at least {min} repetitions (got {got})"));
            return Ok(ParseOutcome::failure(cur));
        }
        Ok(repeat_value(cur, values, "repeat"))
    }

    /// `element (separator element)*`
    pub(super) fn match_sep_one_or_more(
        &mut self,
        pos: usize,
        element: &PegExpr,
        separator: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let first_snapshot = self.spec_checkpoint();
        let first = self.parse_expr(element, pos, active)?;
        let (mut cur, first_val) = match first {
            ParseOutcome::Failure { .. } => {
                self.spec_rollback(first_snapshot);
                return Ok(first);
            }
            ParseOutcome::Success { pos, value, .. } => {
                self.spec_commit(first_snapshot);
                (pos, value)
            }
        };
        let mut values = Vec::with_capacity(4);
        values.push(first_val);
        loop {
            let snapshot = self.spec_checkpoint();
            let sep_result = self.parse_expr(separator, cur, active)?;
            let sep_pos = match sep_result {
                ParseOutcome::Failure { .. } => {
                    self.spec_rollback(snapshot);
                    break;
                }
                ParseOutcome::Success { pos, .. } => pos,
            };
            match self.parse_expr(element, sep_pos, active)? {
                ParseOutcome::Failure { .. } => {
                    self.spec_rollback(snapshot);
                    break;
                }
                ParseOutcome::Success { pos, value, .. } => {
                    self.spec_commit(snapshot);
                    values.push(value);
                    cur = pos;
                }
            }
        }
        Ok(repeat_value(cur, values, "sep_one_or_more"))
    }

    /// `element (separator element)*` — separators are kept in the output list.
    ///
    /// Output: `Node("interspersed", [elem1, sep1, elem2, sep2, ...])`
    pub(super) fn match_interspersed(
        &mut self,
        pos: usize,
        element: &PegExpr,
        separator: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let first_snapshot = self.spec_checkpoint();
        let first = self.parse_expr(element, pos, active)?;
        let (mut cur, first_val) = match first {
            ParseOutcome::Failure { .. } => {
                self.spec_rollback(first_snapshot);
                return Ok(first);
            }
            ParseOutcome::Success { pos, value, .. } => {
                self.spec_commit(first_snapshot);
                (pos, value)
            }
        };
        let mut values = Vec::with_capacity(7);
        values.push(first_val);
        loop {
            let snapshot = self.spec_checkpoint();
            let sep_result = self.parse_expr(separator, cur, active)?;
            let (sep_pos, sep_val) = match sep_result {
                ParseOutcome::Failure { .. } => {
                    self.spec_rollback(snapshot);
                    break;
                }
                ParseOutcome::Success { pos, value, .. } => (pos, value),
            };
            match self.parse_expr(element, sep_pos, active)? {
                ParseOutcome::Failure { .. } => {
                    self.spec_rollback(snapshot);
                    break;
                }
                ParseOutcome::Success { pos, value, .. } => {
                    self.spec_commit(snapshot);
                    values.push(sep_val);
                    values.push(value);
                    cur = pos;
                }
            }
        }
        Ok(repeat_value(cur, values, "interspersed"))
    }

    // ── Value-binding matcher ──────────────────────────────────────────────

    pub(super) fn match_named(
        &mut self,
        pos: usize,
        name: &str,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let result = self.parse_expr(expr, pos, active)?;
        match result {
            ParseOutcome::Success { pos, value, cut } => {
                // Record the bound token text so `backref("name")` can match it.
                if let ParseValue::Text(text) = value.inner() {
                    self.state
                        .captures
                        .insert(name.to_string(), text.to_string());
                }
                Ok(ParseOutcome::Success {
                    pos,
                    value: ParseValue::Named(Arc::from(name), Arc::new(value)),
                    cut,
                })
            }
            other => Ok(other),
        }
    }

    // ── Error-label override ───────────────────────────────────────────────

    pub(super) fn match_expected(
        &mut self,
        pos: usize,
        message: &str,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let before = self.state.diag.furthest;
        let result = self.parse_expr(expr, pos, active)?;
        if result.is_success() {
            return Ok(result);
        }
        let fail_pos = if self.state.diag.furthest > before {
            self.state.diag.furthest
        } else {
            pos
        };
        // Replace whatever was recorded with our label.
        self.state.diag.expected.remove(&fail_pos);
        self.state.diag.record_expected(fail_pos, message);
        Ok(ParseOutcome::failure(fail_pos))
    }

    // ── Trivia control ─────────────────────────────────────────────────────

    pub(super) fn match_no_trivia(
        &mut self,
        pos: usize,
        inner: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let saved = self.state.trivia_on;
        self.state.trivia_on = false;
        let result = self.parse_expr(inner, pos, active);
        self.state.trivia_on = saved;
        result
    }

    /// `with_trivia("spec", e)`: evaluate `e` under the skipper named by `spec`,
    /// then restore the previous skipper. The skipper is built once per spec and
    /// cached (owned by the evaluator).
    pub(super) fn match_with_trivia(
        &mut self,
        pos: usize,
        spec: &str,
        inner: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        if !self.state.trivia_cache.contains_key(spec) {
            let skipper = crate::skip::skip_strategy_from_config(Some(spec)).map_err(|e| {
                ParseError::new(
                    format!("invalid with_trivia spec {spec:?}: {e}"),
                    pos,
                    self.ctx.text.len(),
                )
            })?;
            self.state.trivia_cache.insert(spec.to_string(), skipper);
        }
        self.state.trivia_override_stack.push(spec.to_string());
        let result = self.parse_expr(inner, pos, active);
        self.state.trivia_override_stack.pop();
        result
    }

    // ── Layout-sensitive terminals ─────────────────────────────────────────

    pub(super) fn match_newline(&mut self, pos: usize) -> Result<ParseOutcome, ParseError> {
        // When inside brackets or indentation is disabled, consume optional
        // surrounding whitespace and match \r?\n or \r.
        let cur = if self.state.layout.indentation_enabled && self.state.layout.bracket_depth == 0 {
            // Indent-aware: skip only horizontal whitespace before the newline.
            let mut c = pos;
            while c < self.ctx.text.len() && matches!(self.ctx.text.as_bytes()[c], b' ' | b'\t') {
                c += 1;
            }
            c
        } else {
            self.skip_trivia(pos)?
        };

        let end = match_newline_at(self.ctx.text, cur);
        if let Some(end) = end {
            self.note_read(pos, end);
            self.state.layout.at_line_start = true;
            Ok(ParseOutcome::success(
                end,
                ParseValue::Text(Arc::from("\n")),
            ))
        } else {
            let examined = self.examined_with_lookahead(cur);
            self.note_read(pos, examined);
            self.state.diag.record_expected(cur, "newline");
            Ok(ParseOutcome::failure(cur))
        }
    }

    pub(super) fn match_indent(&mut self, pos: usize) -> Result<ParseOutcome, ParseError> {
        if !self.state.layout.indentation_enabled
            || self.state.layout.bracket_depth > 0
            || !self.state.layout.at_line_start
        {
            self.state.diag.record_expected(pos, "indent");
            return Ok(ParseOutcome::failure(pos));
        }
        let (width, end) = measure_indent(self.ctx.text, pos);
        self.note_read(pos, self.examined_with_lookahead(end));
        let current = *self.state.layout.indent_stack.last().unwrap_or(&0);
        if width <= current {
            self.state.diag.record_expected(pos, "indent");
            return Ok(ParseOutcome::failure(pos));
        }
        self.state.layout.indent_stack.push(width);
        self.state.layout.at_line_start = false;
        Ok(ParseOutcome::success(end, ParseValue::Nil))
    }

    pub(super) fn match_dedent(&mut self, pos: usize) -> Result<ParseOutcome, ParseError> {
        if !self.state.layout.indentation_enabled
            || self.state.layout.bracket_depth > 0
            || !self.state.layout.at_line_start
        {
            self.state.diag.record_expected(pos, "dedent");
            return Ok(ParseOutcome::failure(pos));
        }
        let (width, end) = measure_indent(self.ctx.text, pos);
        self.note_read(pos, self.examined_with_lookahead(end));
        if self.state.layout.indent_stack.len() <= 1 {
            self.state.diag.record_expected(pos, "dedent");
            return Ok(ParseOutcome::failure(pos));
        }
        let target = self.state.layout.indent_stack[self.state.layout.indent_stack.len() - 2];
        if width != target {
            self.state.diag.record_expected(pos, "dedent");
            return Ok(ParseOutcome::failure(pos));
        }
        self.state.layout.indent_stack.pop();
        self.state.layout.at_line_start = false;
        Ok(ParseOutcome::success(end, ParseValue::Nil))
    }

    // ── Semantic action / predicate ────────────────────────────────────────

    pub(super) fn match_semantic_action(
        &mut self,
        pos: usize,
        name: &str,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let result = self.parse_expr(expr, pos, active)?;
        let ParseOutcome::Success {
            pos: end,
            value,
            cut,
        } = result
        else {
            return Ok(result);
        };
        // `@action` is a `SemanticAction` effect; the host driver returns the
        // verdict (typically `Accept(transformed)`), applied uniformly.
        if self.cfg.driver.is_none() {
            return Err(ParseError::new(
                format!("semantic action '{name}' requires a driver"),
                pos,
                end,
            ));
        }
        let snapshot = self.spec_checkpoint();
        let directive = self.raise_effect(crate::driver::ParseEffect::SemanticAction {
            name,
            args: &[],
            pos,
            end,
            value: &value,
        });
        let label = format!("@{name}");
        self.apply_verdict(
            directive,
            snapshot,
            MatchedHook {
                pos,
                end,
                value,
                cut,
            },
            &label,
        )
    }

    pub(super) fn match_semantic_predicate(
        &mut self,
        pos: usize,
        name: &str,
    ) -> Result<ParseOutcome, ParseError> {
        // `@?pred` is a zero-width `SemanticPredicate` effect; the host driver
        // returns `Proceed` to accept or `Reject` to fail.
        if self.cfg.driver.is_none() {
            return Err(ParseError::new(
                format!("semantic predicate '{name}' requires a driver"),
                pos,
                pos,
            ));
        }
        let directive = self.raise_effect(crate::driver::ParseEffect::SemanticPredicate {
            name,
            args: &[],
            pos,
            end: pos,
            value: &ParseValue::Nil,
        });
        let label = format!("predicate '{name}'");
        self.apply_verdict(
            directive,
            self.spec_none(),
            MatchedHook {
                pos,
                end: pos,
                value: ParseValue::Nil,
                cut: false,
            },
            &label,
        )
    }

    // ── Semantic guard (`@!name(e)`) ───────────────────────────────────────

    /// Match `expr`, then ask the host driver whether the match is semantically
    /// valid. With no driver attached the value passes through unchanged.
    pub(super) fn match_semantic_guard(
        &mut self,
        pos: usize,
        name: &str,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let snapshot = self.spec_checkpoint();
        let result = self.parse_expr(expr, pos, active)?;
        let ParseOutcome::Success {
            pos: end,
            value,
            cut,
        } = result
        else {
            self.spec_rollback(snapshot);
            return Ok(result);
        };
        let directive = if self.cfg.driver.is_some() {
            self.raise_effect(crate::driver::ParseEffect::Guard {
                name,
                pos,
                end,
                value: &value,
            })
        } else {
            crate::driver::Directive::Proceed
        };
        let label = format!("@!{name}");
        self.apply_verdict(
            directive,
            snapshot,
            MatchedHook {
                pos,
                end,
                value,
                cut,
            },
            &label,
        )
    }

    // ── Span capture ───────────────────────────────────────────────────────

    pub(super) fn match_capture(
        &mut self,
        pos: usize,
        _label: &str,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let result = self.parse_expr(expr, pos, active)?;
        if let ParseOutcome::Success {
            pos: end,
            value,
            cut,
        } = result
        {
            Ok(ParseOutcome::Success {
                pos: end,
                value: ParseValue::SpannedValue {
                    value: Arc::new(value),
                    start: pos,
                    end,
                },
                cut,
            })
        } else {
            Ok(result)
        }
    }

    // ── Island ─────────────────────────────────────────────────────────────

    /// Match all text between `start` and `end` delimiter strings.
    /// Does NOT recurse (no nesting) — use `raw_block` for that.
    pub(super) fn match_island(
        &mut self,
        pos: usize,
        start: &str,
        end: &str,
        include_delims: bool,
    ) -> Result<ParseOutcome, ParseError> {
        let cur = self.skip_trivia(pos)?;
        if !self.ctx.text[cur..].starts_with(start) {
            self.note_read(cur, (cur + start.len()).min(self.ctx.text.len()));
            self.state.diag.record_expected(cur, format!("'{start}'"));
            return Ok(ParseOutcome::failure(cur));
        }
        let content_start = cur + start.len();
        let Some(end_offset) = self.ctx.text[content_start..].find(end) else {
            // The unsuccessful search scanned the rest of the input.
            self.note_read(cur, self.ctx.text.len());
            self.state
                .diag
                .record_expected(content_start, format!("closing '{end}'"));
            return Ok(ParseOutcome::failure(content_start));
        };
        let content_end = content_start + end_offset;
        let full_end = content_end + end.len();
        self.note_read(cur, full_end);
        let value = if include_delims {
            ParseValue::Text(Arc::from(&self.ctx.text[cur..full_end]))
        } else {
            ParseValue::Text(Arc::from(&self.ctx.text[content_start..content_end]))
        };
        Ok(ParseOutcome::success(full_end, value))
    }

    // ── RawBlock ───────────────────────────────────────────────────────────

    /// Match nested balanced delimiters, yielding the inner content.
    /// Returns a `ParseValue::Text` of the content between the outermost delimiters.
    pub(super) fn match_raw_block(
        &mut self,
        pos: usize,
        start: &str,
        end: &str,
        delim_kind: &str,
    ) -> Result<ParseOutcome, ParseError> {
        // Nesting needs distinguishable open/close delimiters; identical ones
        // can never increase depth, so reject the configuration explicitly
        // rather than silently treating the first close as the match.
        if start == end {
            return Err(ParseError::new(
                format!(
                    "raw_block(kind={delim_kind}) requires distinct start/end delimiters; both are {start:?}"
                ),
                pos,
                self.ctx.text.len(),
            )
            .with_code("raw_block_identical_delimiters"));
        }
        let cur = self.skip_trivia(pos)?;
        if !self.ctx.text[cur..].starts_with(start) {
            self.note_read(cur, (cur + start.len()).min(self.ctx.text.len()));
            self.state.diag.record_expected(cur, format!("'{start}'"));
            return Ok(ParseOutcome::failure(cur));
        }
        // The balanced scan searches forward for delimiters; absent a further
        // open delimiter the search reaches end-of-input, so the examined extent
        // is the whole remainder.
        self.note_read(cur, self.ctx.text.len());
        let content_start = cur + start.len();
        let mut depth: usize = 1;
        let mut scan = content_start;
        let text = self.ctx.text;
        loop {
            let next_open = text[scan..].find(start).map(|o| scan + o);
            let next_close = text[scan..].find(end).map(|o| scan + o);
            match (next_open, next_close) {
                (_, None) => {
                    return Err(ParseError::new(
                        format!("unterminated raw block (kind={delim_kind})"),
                        cur,
                        text.len(),
                    ));
                }
                (Some(open), Some(close)) if open < close => {
                    depth += 1;
                    scan = open + start.len();
                }
                (_, Some(close)) => {
                    depth -= 1;
                    if depth == 0 {
                        let content = text[content_start..close].to_string();
                        let full_end = close + end.len();
                        return Ok(ParseOutcome::success(
                            full_end,
                            ParseValue::Text(content.into()),
                        ));
                    }
                    scan = close + end.len();
                }
            }
        }
    }

    // ── Eager ──────────────────────────────────────────────────────────────

    /// Like a regular expression, but on failure escalates to a fatal `ParseError`
    /// instead of returning a soft `ParseOutcome::Failure`.
    pub(super) fn match_eager(
        &mut self,
        pos: usize,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        match self.parse_expr(expr, pos, active)? {
            ok @ ParseOutcome::Success { .. } => Ok(ok),
            ParseOutcome::Failure { pos: fail_pos, .. } => {
                let expected = self.state.diag.expected_at_furthest();
                let msg = fail_message(&expected, "eager parse failure");
                Err(ParseError::new(msg, fail_pos, self.ctx.text.len()))
            }
        }
    }

    // ── Lookbehind ─────────────────────────────────────────────────────────

    /// `&<e` / `!<e`: assert `e` matches a suffix ending exactly at `pos`.
    /// Scans candidate start positions backward within a bounded window;
    /// consumes no input. Intended for short operands (the step budget caps the
    /// total work either way).
    pub(super) fn match_look_behind(
        &mut self,
        pos: usize,
        expr: &PegExpr,
        negative: bool,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        const LOOKBEHIND_MAX: usize = 256;
        let snapshot = self.spec_checkpoint();
        let floor = pos.saturating_sub(LOOKBEHIND_MAX);
        let mut start = pos;
        let mut matched = false;
        loop {
            if self.ctx.text.is_char_boundary(start) {
                if let ParseOutcome::Success { pos: end, .. } =
                    self.parse_expr(expr, start, active)?
                {
                    if end == pos {
                        matched = true;
                        break;
                    }
                }
            }
            if start <= floor {
                break;
            }
            start -= 1;
        }
        // Lookbehind is an assertion: discard any host state it touched.
        self.spec_rollback(snapshot);
        if matched != negative {
            Ok(ParseOutcome::success(pos, ParseValue::Nil))
        } else {
            let label = if negative {
                "no lookbehind match"
            } else {
                "lookbehind match"
            };
            self.state.diag.record_expected(pos, label);
            Ok(ParseOutcome::failure(pos))
        }
    }

    // ── Backref ────────────────────────────────────────────────────────────

    /// `backref("name")`: match input text equal to the most recently captured
    /// `name:` binding text.
    pub(super) fn match_backref(
        &mut self,
        pos: usize,
        name: &str,
    ) -> Result<ParseOutcome, ParseError> {
        let pos = self.skip_trivia(pos)?;
        if let Some(expected) = self.state.captures.get(name).cloned() {
            self.note_read(pos, (pos + expected.len()).min(self.ctx.text.len()));
            if !expected.is_empty() && self.ctx.text[pos..].starts_with(&expected) {
                let end = pos + expected.len();
                return Ok(ParseOutcome::success(
                    end,
                    ParseValue::Text(Arc::from(expected.as_str())),
                ));
            }
        }
        self.state
            .diag
            .record_expected_lazy(pos, || format!("backref '{name}'"));
        Ok(ParseOutcome::failure(pos))
    }

    // ── ImportedRef ────────────────────────────────────────────────────────

    pub(super) fn match_imported_ref(
        &mut self,
        pos: usize,
        grammar_name: &str,
        rule_name: &str,
    ) -> Result<ParseOutcome, ParseError> {
        self.run_in_imported(pos, grammar_name, |sub| {
            if !sub.ctx.rules.contains_key(rule_name) {
                return Err(ParseError::new(
                    format!("rule '{rule_name}' not found in imported grammar '{grammar_name}'"),
                    pos,
                    sub.ctx.text.len(),
                ));
            }
            let mut active = HashSet::new();
            sub.parse_rule_inner(rule_name, pos, &mut active)
        })
    }

    /// Build a sub-evaluator over an imported grammar's compiled context, run
    /// `body` inside it, and propagate furthest-failure diagnostics back to the
    /// parent. Text, trivia config, lex tokens, and indentation state are shared
    /// with the parent, but the sub-evaluator gets its own memo cache and active
    /// set. Shared by `match_imported_ref` and `match_grammar_scope`.
    fn run_in_imported<F>(
        &mut self,
        pos: usize,
        grammar_name: &str,
        body: F,
    ) -> Result<ParseOutcome, ParseError>
    where
        F: for<'s> FnOnce(&mut PegEvaluator<'s>) -> Result<ParseOutcome, ParseError>,
    {
        let imported = match self.ctx.imports.get(grammar_name) {
            Some(c) => c,
            None => {
                return Err(ParseError::new(
                    format!("unknown grammar import '{grammar_name}' — add it to the grammar registry via Grammar::add_import()"),
                    pos,
                    self.ctx.text.len(),
                ));
            }
        };

        let indentation_enabled = self.state.layout.indentation_enabled;
        let trivia_clone = self.cfg.trivia.clone();
        let lex_tokens = self.cfg.lex_tokens.clone();
        let mut sub = PegEvaluator::new(PegEvaluatorInit {
            ctx: ParseCtx::from_compiled(imported, self.ctx.text),
            cfg: EvalCfg::new(
                self.cfg.invalid_rule_policy.clone(),
                trivia_clone,
                MemoRuntimeConfig {
                    enabled: self.cfg.use_memo,
                    rule_limit: self.cfg.memo_rule_limit,
                },
                self.cfg.config_context.clone(),
            ),
            indentation_enabled,
        });
        sub.cfg.lex_tokens = lex_tokens;
        sub.cfg.driver = self.cfg.driver;
        sub.cfg.track_read_extent = self.cfg.track_read_extent;
        // Mirror trivia state from parent.
        sub.state.trivia_on = self.state.trivia_on;

        // Capture everything the imported parse examines (its own read stack is
        // independent) and bubble it into the caller's frame so a rule spanning
        // an import boundary records a sound read extent.
        sub.push_read_frame(pos);
        let outcome = body(&mut sub)?;
        sub.pop_read_frame();
        self.merge_read(sub.state.last_read_extent);
        // Propagate furthest-failure diagnostics back to the parent.
        if sub.state.diag.furthest >= self.state.diag.furthest {
            self.state.diag.furthest = sub.state.diag.furthest;
            self.state.diag.expected = sub.state.diag.expected.clone();
        }
        Ok(outcome)
    }

    // ── Parameters ────────────────────────────────────────────────────────

    pub(super) fn match_parameter(
        &mut self,
        pos: usize,
        name: &str,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        // Walk the params stack from top to bottom.
        for frame in self.state.params.iter().rev() {
            if let Some(expr) = frame.get(name) {
                let expr = expr.clone();
                return self.parse_expr_owned(expr, pos, active);
            }
        }
        Err(ParseError::new(
            format!("unbound parameter '${name}'"),
            pos,
            pos,
        ))
    }

    pub(super) fn match_call(
        &mut self,
        pos: usize,
        rule: &str,
        args: &[PegExpr],
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        // Look up parameter names for this rule.
        let rule_params = self.ctx.rule_params;
        let param_names: Vec<String> = rule_params.get(rule).cloned().unwrap_or_default();

        // Build a parameter frame: name → owned arg PegExpr. A `$param` argument
        // is resolved against the CALLER's frames first, so it carries the
        // caller's binding rather than re-referencing the callee's own param of
        // the same name (which would recurse forever).
        let frame: HashMap<String, PegExpr> = param_names
            .iter()
            .zip(args.iter())
            .map(|(name, arg)| (name.clone(), self.resolve_arg_param(arg)))
            .collect();

        self.state.params.push(frame);
        let result = self.parse_rule_inner(rule, pos, active);
        self.state.params.pop();
        result
    }

    /// Resolve a call argument that is a bare `$param` against the current
    /// (caller's) parameter frames, so the binding passed to the callee is the
    /// caller's value, not a self-reference. Non-parameter args pass through.
    fn resolve_arg_param(&self, arg: &PegExpr) -> PegExpr {
        if let PegExpr::Parameter { name } = arg {
            for frame in self.state.params.iter().rev() {
                if let Some(expr) = frame.get(name) {
                    return expr.clone();
                }
            }
        }
        arg.clone()
    }

    // ── Keyword terminals ─────────────────────────────────────────────────

    pub(super) fn match_hard_keyword(
        &mut self,
        pos: usize,
        kw: &str,
    ) -> Result<ParseOutcome, ParseError> {
        let pos = self.skip_trivia(pos)?;
        let suffix = &self.ctx.text[pos..];
        if !suffix.starts_with(kw) {
            self.note_read(pos, (pos + kw.len()).min(self.ctx.text.len()));
            self.state
                .diag
                .record_expected(pos, format!("keyword '{kw}'"));
            return Ok(ParseOutcome::failure(pos));
        }
        let end = pos + kw.len();
        // Hard keyword: must not be followed by an identifier character. The
        // trailing-char inspection extends the examined extent one char past the
        // keyword.
        self.note_read(pos, self.examined_with_lookahead(end));
        let next = self.ctx.text[end..].chars().next();
        if next
            .map(|c| c.is_alphanumeric() || c == '_')
            .unwrap_or(false)
        {
            self.state
                .diag
                .record_expected(pos, format!("keyword '{kw}'"));
            return Ok(ParseOutcome::failure(pos));
        }
        Ok(ParseOutcome::success(end, ParseValue::Text(Arc::from(kw))))
    }

    pub(super) fn match_soft_keyword(
        &mut self,
        pos: usize,
        kw: &str,
    ) -> Result<ParseOutcome, ParseError> {
        // Soft keywords behave like HardKeyword (word-boundary check) since
        // their contextual nature is enforced by grammar structure, not a
        // runtime keyword list.  This is consistent with how most PEG parsers
        // handle contextual keywords.
        self.match_hard_keyword(pos, kw)
    }

    // ── Token-stream matching ──────────────────────────────────────────────

    pub(super) fn match_token_ref(
        &mut self,
        pos: usize,
        kind: &Option<String>,
        text: &Option<String>,
    ) -> Result<ParseOutcome, ParseError> {
        let pos = self.skip_trivia(pos)?;
        let tokens = match &self.cfg.lex_tokens {
            Some(t) => t.clone(),
            None => {
                return Err(ParseError::new(
                    "tok() expression requires a token stream; attach one via ParseRequest::tokens() or ParseRequest::scan()",
                    pos,
                    self.ctx.text.len(),
                ));
            }
        };
        let label = format_tok_label(kind, text);
        // Find the leftmost token that starts exactly at the current parser
        // position. Matching from the middle of a token would overlap text that
        // another grammar expression already consumed.
        let idx = tokens.partition_point(|t| t.start < pos);
        let tok = if idx < tokens.len() && tokens[idx].start == pos {
            &tokens[idx]
        } else {
            self.note_read(pos, pos);
            self.state.diag.record_expected(pos, label);
            return Ok(ParseOutcome::failure(pos));
        };
        // The token at `pos` was inspected regardless of whether it matched.
        let tok_end = tok.end;
        self.note_read(pos, tok_end);
        if let Some(k) = kind {
            if &tok.kind != k {
                self.state.diag.record_expected(pos, label);
                return Ok(ParseOutcome::failure(pos));
            }
        }
        if let Some(t) = text {
            if &tok.text != t {
                self.state.diag.record_expected(pos, label);
                return Ok(ParseOutcome::failure(pos));
            }
        }
        Ok(ParseOutcome::success(
            tok_end,
            ParseValue::Text(Arc::from(tok.text.as_str())),
        ))
    }

    // ── Grammar scope ──────────────────────────────────────────────────────

    pub(super) fn match_grammar_scope(
        &mut self,
        pos: usize,
        grammar_name: &str,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let _ = active; // parent active set unchanged — scope is its own call frame
        self.run_in_imported(pos, grammar_name, |sub| {
            let mut sub_active = HashSet::new();
            sub.parse_expr(expr, pos, &mut sub_active)
        })
    }
}
